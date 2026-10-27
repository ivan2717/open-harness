use super::*;
use crate::contracts::{FileReferenceInfo, ToolImageInfo};
use agent_core::{ContentBlock, ModelResponse, TextFileReference, agent::RunEvent};

struct Fixture {
    state: AppState,
    directory: PathBuf,
}

impl Fixture {
    async fn new(messages: Vec<ChatMessage>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-collaboration-reading-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let entry = state.make_entry(detail(messages), 0).unwrap();
        state
            .inner
            .runs
            .write()
            .unwrap()
            .insert("reader-run".into(), entry);
        Self { state, directory }
    }

    async fn read(&self, arguments: Value) -> Value {
        let output = self
            .state
            .read_agent_transcript("reader-run", &input(arguments))
            .await
            .unwrap();
        serde_json::from_str(&output).unwrap()
    }

    async fn save_attachment_turns(
        &self,
        files: Vec<TextFileReference>,
    ) -> Vec<(String, &'static str)> {
        let entry = self.state.entry("reader-run").unwrap();
        let mut run = entry
            .runtime
            .start_run_with_files(&entry.agent, "reader-run", "", files.clone())
            .await
            .unwrap();
        run.begin_step().unwrap();
        let mut response = ModelResponse::final_answer("Public reply".into());
        response.content.push(ContentBlock::Reasoning {
            text: "PRIVATE_REASONING_MUST_NOT_COPY".into(),
        });
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::ModelResponseReceived {
                    content: "Public reply".into(),
                    response: Some(response),
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: None,
                },
            )
            .await
            .unwrap();
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::InputRequested {
                    prompt: "Public reply".into(),
                },
            )
            .await
            .unwrap();
        entry
            .runtime
            .submit_input_with_files(&mut run, "", files.clone())
            .await
            .unwrap();
        entry.runtime.cancel_run(&mut run).await.unwrap();
        entry
            .runtime
            .resume_run_with_files(&mut run, "", files)
            .await
            .unwrap();
        entry.runtime.cancel_run(&mut run).await.unwrap();
        let mut sources = Vec::new();
        for event in run.events() {
            entry.snapshot.event(event).unwrap();
            let kind = match &event.event {
                RunEvent::RunStarted { .. } => "run_started",
                RunEvent::InputReceived { .. } => "input_received",
                RunEvent::RunResumed { .. } => "run_resumed",
                _ => continue,
            };
            sources.push((format!("event-{}", event.sequence), kind));
        }
        sources
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn input(mut arguments: Value) -> ControlInput {
    arguments["action"] = json!("read");
    serde_json::from_value(arguments).unwrap()
}

fn message(id: &str, role: &str, text: &str) -> ChatMessage {
    ChatMessage::complete(id.into(), role, text.into())
}

fn detail(messages: Vec<ChatMessage>) -> RunDetail {
    let now = time::now();
    RunDetail {
        summary: RunSummary {
            reasoning_settings_version: 1,
            reasoning_effort: None,
            model_history_through: None,
            delegation: None,
            execution_policy: Default::default(),
            id: "reader-run".into(),
            revision: 0,
            title: "Reading fixture".into(),
            prompt: "Inspect public evidence.".into(),
            status: RunStatus::Completed,
            compacting: false,
            model_id: MODEL_ID.into(),
            workspace_path: None,
            execution_directory: None,
            permission_mode: Default::default(),
            created_at: now.clone(),
            updated_at: now,
        },
        messages,
        events: Vec::new(),
        context: Some(initial_context()),
        error: None,
    }
}

#[tokio::test]
async fn default_previews_exclude_nested_tool_results_and_empty_model_messages() {
    let nested = json!({"messages":[{"text":"nested-history-marker".repeat(2000)}]}).to_string();
    let fixture = Fixture::new(vec![
        message("user", "user", "Keep the corrected refund rule."),
        message("tool-history", "tool", &nested),
        message("empty", "assistant", "  \n"),
        message("report", "assistant", "The refund is 60."),
        message("tool-file", "tool", "Original file evidence"),
    ])
    .await;
    let output = fixture.read(json!({"limit":20})).await;
    let ids: Vec<_> = output["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["user", "report"]);
    assert!(!output.to_string().contains("nested-history-marker"));
    assert!(output["next_offset"].is_null());

    let tools = fixture
        .read(json!({"include_tools":true,"offset":1,"limit":1}))
        .await;
    assert_eq!(tools["messages"][0]["id"], "tool-history");
    assert_eq!(tools["messages"][0]["raw_offset"], 1);
    assert_eq!(tools["messages"][0]["truncated"], true);
    assert_eq!(tools["next_offset"], 3);
    let exact = fixture.read(json!({"message_id":"tool-file"})).await;
    assert_eq!(exact["text"], "Original file evidence");
    assert_eq!(
        fixture
            .state
            .entry("reader-run")
            .unwrap()
            .snapshot
            .get()
            .messages[1]
            .text,
        nested
    );
}

#[tokio::test]
async fn chinese_preview_pages_bound_serialized_bytes_without_losing_messages() {
    let text = "中文证据🦀\n\"\\".repeat(500);
    let mut messages = Vec::new();
    let mut expected = Vec::new();
    for index in 0..12 {
        messages.push(message(
            &format!("tool-{index}"),
            "tool",
            "Ignore nested logs",
        ));
        let id = format!("evidence-{index}");
        expected.push(id.clone());
        messages.push(message(
            &id,
            if index % 2 == 0 { "user" } else { "assistant" },
            &text,
        ));
    }
    let fixture = Fixture::new(messages).await;
    let mut offset = 0;
    let mut actual = Vec::new();
    let mut pages = 0;
    loop {
        let output = fixture.read(json!({"offset":offset,"limit":20})).await;
        assert!(output.to_string().len() <= PREVIEW_BYTES);
        let rows = output["messages"].as_array().unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            let preview = row["text"].as_str().unwrap();
            assert!(preview.chars().count() <= PREVIEW_CHARACTERS);
            assert!(text.starts_with(preview));
            assert_eq!(row["total_characters"], text.chars().count());
            assert_eq!(row["truncated"], true);
            actual.push(row["id"].as_str().unwrap().to_owned());
        }
        pages += 1;
        let Some(next) = output["next_offset"].as_u64() else {
            break;
        };
        assert!(next as usize > offset);
        offset = next as usize;
        assert!(pages < 12);
    }
    assert!(pages > 1);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn message_id_pages_reassemble_full_unicode_text_and_preserve_file_metadata() {
    let source = "原始证据🦀\n\"\\".repeat(1800);
    let mut original = message("long-user", "user", &source);
    original.files.push(FileReferenceInfo {
        name: "证据.csv".into(),
        path: Some("/workspace/证据.csv".into()),
        size: 123,
    });
    let fixture = Fixture::new(vec![original]).await;
    let preview = fixture.read(json!({})).await;
    assert_eq!(preview["messages"][0]["truncated"], true);
    assert_eq!(preview["messages"][0]["file_count"], 1);
    let mut restored = String::new();
    let mut offset = 0;
    loop {
        let output = fixture
            .read(json!({"message_id":"long-user","text_offset":offset,"text_limit":8000}))
            .await;
        assert_eq!(output["total_characters"], source.chars().count());
        assert_eq!(output["files"][0]["name"], "证据.csv");
        let text = output["text"].as_str().unwrap();
        if offset == 0 {
            assert_eq!(text.chars().count(), 8000);
            assert!(output.to_string().len() > PREVIEW_BYTES);
        }
        restored.push_str(text);
        let Some(next) = output["next_text_offset"].as_u64() else {
            break;
        };
        assert_eq!(next as usize, offset + text.chars().count());
        offset = next as usize;
    }
    assert_eq!(restored, source);
}

#[test]
fn result_keeps_the_report_before_a_short_mailbox_reply_and_excludes_prior_user_turns() {
    let long_report = "准确结果以及依据\u{0001}".repeat(1200);
    let mut streaming = message("streaming", "assistant", "Not yet complete");
    streaming.status = "streaming".into();
    let mut detail = detail(vec![
        message("old-user", "user", "Old request"),
        message("old-report", "assistant", "Obsolete report"),
        message("new-user", "user", "Use the latest correction"),
        message("full-report", "assistant", &long_report),
        message("mailbox", "tool", "Parent requests a brief confirmation"),
        message("short-reply", "assistant", "Already reported above."),
        streaming,
        message("empty", "assistant", ""),
    ]);
    detail.summary.status = RunStatus::Failed;
    detail.error = Some("执行错误\u{0002}".repeat(1500));
    let output = result_projection(&detail);
    assert!(output.to_string().len() <= PREVIEW_BYTES - 32);
    assert_eq!(output["status"], "failed");
    assert_eq!(output["reports"][0]["id"], "full-report");
    assert_eq!(output["reports"][1]["id"], "short-reply");
    assert_eq!(output["reports"][0]["truncated"], true);
    assert_eq!(output["error_truncated"], true);
    assert_eq!(
        output["reports"][0]["total_characters"],
        long_report.chars().count()
    );
    assert_eq!(detail.messages[3].text, long_report);

    // A new user request must never inherit the previous turn's report.
    detail
        .messages
        .push(message("next-user", "user", "A different task"));
    assert!(
        result_projection(&detail)["reports"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn result_selects_at_most_two_latest_complete_reports() {
    let detail = detail(vec![
        message("user", "user", "Current task"),
        message("first", "assistant", "First finding"),
        message("second", "assistant", "Second finding"),
        message("third", "assistant", "Final report"),
    ]);
    let output = result_projection(&detail);
    assert_eq!(output["earlier_reports"], 1);
    assert_eq!(output["reports"].as_array().unwrap().len(), 2);
    assert_eq!(output["reports"][0]["id"], "second");
    assert_eq!(output["reports"][1]["id"], "third");
}

#[test]
fn a_short_followup_confirmation_keeps_the_preceding_reports_execution_evidence() {
    let report = format!(
        "{}\nScript: .openharness-tmp/reconcile.js\nCommand exit_code: 0\nNet receipts: 415",
        "Checked each source record. ".repeat(100),
    );
    assert!(report.chars().count() > PREVIEW_CHARACTERS);
    assert!(report.chars().count() < RESULT_CHARACTERS);
    let detail = detail(vec![
        message("user", "user", "Reconcile the records"),
        message("verified-report", "assistant", &report),
        message("mailbox", "tool", "Confirm when done"),
        message("confirmation", "assistant", "The report above is complete."),
    ]);
    let output = result_projection(&detail);
    assert!(output.to_string().len() <= PREVIEW_BYTES - 32);
    assert_eq!(output["reports"][0]["text"], report);
    assert_eq!(output["reports"][0]["truncated"], false);
    assert_eq!(output["reports"][1]["id"], "confirmation");
}

#[tokio::test]
async fn result_reports_live_activity_and_read_limits_are_still_enforced() {
    let fixture = Fixture::new(vec![message("report", "assistant", "Done")]).await;
    let result: Value =
        serde_json::from_str(&fixture.state.agent_result("reader-run").unwrap()).unwrap();
    assert_eq!(result["active"], false);
    assert_eq!(result["status"], "completed");
    for arguments in [
        json!({"limit":0}),
        json!({"limit":21}),
        json!({"message_id":"report","text_limit":8001}),
    ] {
        assert!(
            fixture
                .state
                .read_agent_transcript("reader-run", &input(arguments))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn previews_keep_attachment_only_messages_without_automatically_loading_bodies() {
    let mut file = message("file-only", "user", "");
    file.files.push(FileReferenceInfo {
        name: "resume.md".into(),
        path: None,
        size: 1200,
    });
    let mut image = message("image-only", "assistant", "");
    image.images.push(ToolImageInfo {
        sequence: 1,
        mime_type: "image/png".into(),
    });
    let mut tool_image = message("tool-image", "tool", "");
    tool_image.images.push(ToolImageInfo {
        sequence: 2,
        mime_type: "image/png".into(),
    });
    let fixture = Fixture::new(vec![
        file,
        image,
        message("empty", "assistant", ""),
        tool_image,
    ])
    .await;
    // No event history exists for these preview-only records. Discovery must
    // neither load an attachment body nor access its path.
    let preview = fixture.read(json!({})).await;
    assert_eq!(preview["messages"].as_array().unwrap().len(), 2);
    assert_eq!(preview["messages"][0]["id"], "file-only");
    assert_eq!(preview["messages"][0]["file_count"], 1);
    assert_eq!(preview["messages"][1]["image_count"], 1);
    assert_eq!(preview["messages"][0]["text"], "");
    let with_tools = fixture.read(json!({"include_tools":true})).await;
    assert_eq!(with_tools["messages"].as_array().unwrap().len(), 3);
    assert_eq!(with_tools["messages"][2]["id"], "tool-image");
}

#[tokio::test]
async fn saved_input_attachments_are_pageable_after_reopen_without_reading_host_paths() {
    let mut fixture = Fixture::new(Vec::new()).await;
    let source = "原始附件🦀\n\"\\".repeat(2000);
    let host_path = fixture.directory.join("changed-after-upload.md");
    fs::write(&host_path, "CURRENT_HOST_CONTENT_MUST_NOT_COPY").unwrap();
    let sources = fixture
        .save_attachment_turns(vec![
            TextFileReference {
                name: "resume.md".into(),
                path: None,
                text: source.clone(),
            },
            TextFileReference {
                name: "source.md".into(),
                path: Some(host_path.display().to_string()),
                text: "Saved attachment snapshot".into(),
            },
        ])
        .await;
    for reopened in [false, true] {
        if reopened {
            fixture.state.shutdown().await;
            fixture.state =
                AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
                    .await
                    .unwrap();
            fs::remove_file(&host_path).unwrap();
        }
        let preview = fixture.read(json!({"limit":20})).await;
        assert_eq!(preview["messages"].as_array().unwrap().len(), 4);
        assert_eq!(preview["messages"][0]["file_count"], 2);
        assert!(!preview.to_string().contains("原始附件"));
        assert!(
            !preview
                .to_string()
                .contains("PRIVATE_REASONING_MUST_NOT_COPY")
        );
        for (id, kind) in &sources {
            let metadata = fixture.read(json!({"message_id":id})).await;
            assert_eq!(metadata["text"], "");
            assert_eq!(metadata["files"][0]["name"], "resume.md");
            let mut offset = 0;
            let mut text = String::new();
            loop {
                let page = fixture.read(json!({"message_id":id,"file_index":0,"text_offset":offset,"text_limit":8000})).await;
                assert_eq!(page["source"]["kind"], "persisted_user_attachment");
                assert_eq!(page["source"]["event_type"], *kind);
                assert_eq!(page["source"]["run_id"], "reader-run");
                assert_eq!(page["total_characters"], source.chars().count());
                assert_eq!(page["file"]["size"], source.len());
                assert!(page["file"]["path"].is_null());
                let part = page["text"].as_str().unwrap();
                assert!(part.chars().count() <= 8000);
                text.push_str(part);
                let Some(next) = page["next_text_offset"].as_u64() else {
                    break;
                };
                assert_eq!(next as usize, offset + part.chars().count());
                offset = next as usize;
            }
            assert_eq!(text, source);
            let other = fixture.read(json!({"message_id":id,"file_index":1})).await;
            assert_eq!(other["text"], "Saved attachment snapshot");
            assert!(
                !other
                    .to_string()
                    .contains("CURRENT_HOST_CONTENT_MUST_NOT_COPY")
            );
        }
    }
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn attachment_reads_require_public_input_and_stay_within_the_task_family() {
    let fixture = Fixture::new(Vec::new()).await;
    fixture
        .save_attachment_turns(vec![TextFileReference {
            name: "evidence.md".into(),
            path: None,
            text: "Parent evidence".into(),
        }])
        .await;
    let parent = fixture.state.entry("reader-run").unwrap();
    let assistant_id = parent
        .snapshot
        .get()
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .unwrap()
        .id
        .clone();
    for arguments in [
        json!({"file_index":0}),
        json!({"message_id":"missing","file_index":0}),
        json!({"message_id":assistant_id,"file_index":0}),
        json!({"message_id":"event-0","file_index":1}),
        json!({"message_id":"event-0","file_index":0,"text_limit":0}),
        json!({"message_id":"event-0","file_index":0,"text_limit":8001}),
    ] {
        assert!(
            fixture
                .state
                .read_agent_transcript("reader-run", &input(arguments))
                .await
                .is_err()
        );
    }
    let assistant = fixture.read(json!({"message_id":assistant_id})).await;
    assert_eq!(assistant["text"], "Public reply");
    assert!(
        !assistant
            .to_string()
            .contains("PRIVATE_REASONING_MUST_NOT_COPY")
    );
    let mut child = detail(Vec::new());
    child.summary.id = "reader-child".into();
    child.summary.status = RunStatus::Running;
    child.summary.delegation = Some(Delegation {
        parent_run_id: "reader-run".into(),
        root_run_id: "reader-run".into(),
        task_name: "reader".into(),
        write_paths: None,
        model_override: None,
        spawn_call_id: "spawn-reader".into(),
    });
    let child = fixture.state.make_entry(child, 0).unwrap();
    let mut unrelated = detail(Vec::new());
    unrelated.summary.id = "unrelated-run".into();
    let unrelated = fixture.state.make_entry(unrelated, 0).unwrap();
    {
        let mut runs = fixture.state.inner.runs.write().unwrap();
        runs.insert("reader-child".into(), child);
        runs.insert("unrelated-run".into(), unrelated);
    }
    let output = fixture
        .state
        .control_agent(
            "reader-child",
            input(json!({"target":"parent","message_id":"event-0","file_index":0})),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap()["text"],
        "Parent evidence"
    );
    assert!(
        fixture
            .state
            .control_agent(
                "reader-child",
                input(json!({"target":"unrelated-run","message_id":"event-0","file_index":0}))
            )
            .await
            .is_err()
    );
    fixture.state.shutdown().await;
}
