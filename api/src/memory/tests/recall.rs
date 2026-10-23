use super::*;
use agent_memory::{
    WikiEvidence, WikiPageUpdate, WikiTaskItem, WikiTaskKind, WikiTaskStatus, WikiUpdate,
};

const RUN: &str = "selective-recall";

fn save(
    host: &ExtensionHost,
    events: &[RunEventEnvelope],
    overview: &str,
    pages: Vec<WikiPageUpdate>,
    tasks: Vec<WikiTaskItem>,
) {
    host.memory
        .apply(
            RUN,
            events,
            events.last().unwrap().sequence,
            WikiUpdate {
                overview: overview.into(),
                pages,
                tasks,
            },
        )
        .unwrap();
}

fn page(id: &str, title: &str, content: &str, sequence: u64) -> WikiPageUpdate {
    WikiPageUpdate {
        id: id.into(),
        title: title.into(),
        content: content.into(),
        source_sequences: vec![sequence],
    }
}

fn constraint(id: &str, text: &str, status: WikiTaskStatus, sequence: u64) -> WikiTaskItem {
    WikiTaskItem {
        id: id.into(),
        kind: WikiTaskKind::Constraint,
        status,
        text: text.into(),
        evidence: vec![WikiEvidence {
            sequence,
            quote: text.into(),
        }],
    }
}

fn recalled(host: Arc<ExtensionHost>, events: &[RunEventEnvelope]) -> String {
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host, fixture.clone());
    let messages = memory.messages(input(RUN, events).as_ref());
    assert_eq!(messages.len(), 1);
    let text = messages[0].text();
    assert!(text.chars().count() <= 2_400);
    assert!(
        fixture.requests().is_empty(),
        "recall must not call a model"
    );
    text
}

#[test]
fn relevant_failure_reason_is_recalled_from_the_later_paragraph() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let mut events = source_history("调查 tokenizer 在离线环境下不可用的原因。");
    user(&mut events, "请解释 tokenizer 失败原因和最终选择。");
    let introduction = format!(
        "UNRELATED_LONG_INTRODUCTION {}",
        "项目背景材料。".repeat(100)
    );
    let rationale = "tokenizer 失败原因：部署环境无法下载词表，所以选择本地估算；没有验证过在线环境，不能推断在线环境也不可用。[event #0]";
    save(
        &host,
        &events,
        "OVERVIEW_MUST_NOT_REPLACE_MATCHING_REASON",
        vec![page(
            "token-counting",
            "计数方案",
            &format!("{introduction}\n\n{rationale}"),
            0,
        )],
        vec![],
    );

    let text = recalled(host, &events);
    assert!(
        text.contains(rationale),
        "the complete matched paragraph is useful evidence"
    );
    assert!(!text.contains("UNRELATED_LONG_INTRODUCTION"));
    assert!(!text.contains("OVERVIEW_MUST_NOT_REPLACE_MATCHING_REASON"));
}

#[test]
fn unrelated_overview_pages_and_task_bodies_are_not_automatically_injected() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let mut events = source_history("Document export previously failed.");
    append(
        &mut events,
        RunEvent::ToolCallRequested {
            call_id: "export".into(),
            tool_name: "export_document".into(),
            arguments: "{}".into(),
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallFailed {
            call_id: "export".into(),
            message: "DOCUMENT_EXPORT_FAILURE_PRIVATE_DETAIL".into(),
        },
    );
    user(&mut events, "Analyze postgres indexing.");
    save(
        &host,
        &events,
        "DUPLICATE_PROGRESS_OVERVIEW",
        vec![page(
            "document-layout",
            "Document layout",
            "UNRELATED_LAYOUT_BODY",
            0,
        )],
        vec![WikiTaskItem {
            id: "export-failure".into(),
            kind: WikiTaskKind::Work,
            status: WikiTaskStatus::Failed,
            text: "DOCUMENT_EXPORT_FAILURE_PRIVATE_DETAIL".into(),
            evidence: vec![WikiEvidence {
                sequence: 2,
                quote: "DOCUMENT_EXPORT_FAILURE_PRIVATE_DETAIL".into(),
            }],
        }],
    );

    let text = recalled(host, &events);
    assert!(text.contains("session_memory"));
    assert!(!text.contains("DUPLICATE_PROGRESS_OVERVIEW"));
    assert!(!text.contains("UNRELATED_LAYOUT_BODY"));
    assert!(!text.contains("DOCUMENT_EXPORT_FAILURE_PRIVATE_DETAIL"));
}

#[test]
fn a_current_constraint_keeps_its_complete_negative_qualification() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let requirement = "允许读取现有文件，但不允许上传文件、修改原始数据或调用需要付费的服务；只有用户明确同意后才能执行这些操作。";
    let mut events = source_history(requirement);
    user(&mut events, "分析这份项目资料。");
    save(
        &host,
        &events,
        "The task continues.",
        vec![],
        vec![constraint(
            "data-policy",
            requirement,
            WikiTaskStatus::Active,
            0,
        )],
    );

    let text = recalled(host, &events);
    assert!(text.contains(requirement));
    assert!(text.contains("data-policy"));
}

#[test]
fn a_superseded_constraint_does_not_return_as_an_active_reminder() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let old = "OLD_TOKENIZER_POLICY 必须采用 tokenizer。";
    let current = "NEW_TOKENIZER_POLICY 不要引入 tokenizer，也不要调用计数 API。";
    let mut events = source_history(old);
    user(&mut events, current);
    user(&mut events, "继续研究 tokenizer 方案。");
    save(
        &host,
        &events,
        "The old tokenizer policy was replaced.",
        vec![],
        vec![
            constraint("old-policy", old, WikiTaskStatus::Superseded, 0),
            constraint("current-policy", current, WikiTaskStatus::Active, 1),
        ],
    );

    let text = recalled(host, &events);
    assert!(text.contains(current));
    assert!(!text.contains(old));
}

#[test]
fn continue_uses_the_latest_substantive_user_topic() {
    for follow_up in ["继续", "continue"] {
        let dir = Directory::new();
        let host = ExtensionHost::open(&dir.0).unwrap();
        let mut events = source_history("Investigate postgres retry behavior.");
        user(&mut events, follow_up);
        let failure = "postgres retry exhausted because the transaction was already aborted; start a new transaction before retrying. [event #0]";
        save(
            &host,
            &events,
            "The task is ongoing.",
            vec![page("postgres-retry", "Database recovery", failure, 0)],
            vec![],
        );

        assert!(
            recalled(host, &events).contains(failure),
            "follow-up: {follow_up}"
        );
    }
}

fn read_all(host: &ExtensionHost, events: &[RunEventEnvelope], action: &str) -> String {
    let mut offset = 0;
    let mut revision = None;
    let mut result = String::new();
    loop {
        let mut args = json!({"action": action, "offset": offset});
        if action == "read" {
            args["page_id"] = json!("large-evidence");
        } else {
            args["sequence"] = json!(0);
        }
        if let Some(revision) = revision {
            args["revision"] = json!(revision);
        }
        let response: Value = serde_json::from_str(
            &host
                .memory
                .query_validated(RUN, events, &args.to_string())
                .unwrap(),
        )
        .unwrap();
        revision = response["revision"].as_u64();
        result.push_str(response["content"].as_str().unwrap());
        let Some(next) = response["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    result
}

#[test]
fn omitted_long_paragraphs_remain_fully_readable_with_original_evidence_after_restart() {
    let dir = Directory::new();
    let source = format!(
        "postgres incident evidence. {} END_OF_ORIGINAL_EVIDENCE",
        "observed transaction state; ".repeat(170)
    );
    let mut events = source_history(&source);
    user(&mut events, "Investigate postgres incident evidence.");
    let content = format!(
        "postgres incident rationale. {} END_OF_ARCHIVED_RATIONALE [event #0]",
        "Detailed verified observation; ".repeat(170)
    );
    {
        let host = ExtensionHost::open(&dir.0).unwrap();
        save(
            &host,
            &events,
            "Archived investigation.",
            vec![page("large-evidence", "postgres incident", &content, 0)],
            vec![],
        );
        let text = recalled(host, &events);
        assert!(text.contains("large-evidence"));
        assert!(text.contains("session_memory"));
        assert!(!text.contains("END_OF_ARCHIVED_RATIONALE"));
        assert!(
            !text.contains("Detailed verified observation;"),
            "do not inject a misleading partial paragraph"
        );
    }

    let restored = ExtensionHost::open(&dir.0).unwrap();
    assert_eq!(read_all(&restored, &events, "read"), content);
    assert_eq!(read_all(&restored, &events, "source"), source);
}

#[test]
fn constraints_that_do_not_fit_the_reminder_remain_available_through_tasks() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let requirements: Vec<_> = (0..8)
        .map(|index| {
            format!(
                "POLICY_{index}_START {} POLICY_{index}_END",
                "Read access only; do not alter the source. ".repeat(8)
            )
        })
        .collect();
    let mut events = source_history(&requirements.join("\n"));
    user(&mut events, "Inspect the project.");
    save(
        &host,
        &events,
        "Current policies.",
        vec![],
        requirements
            .iter()
            .enumerate()
            .map(|(index, text)| {
                constraint(&format!("policy-{index}"), text, WikiTaskStatus::Active, 0)
            })
            .collect(),
    );

    let text = recalled(host.clone(), &events);
    assert!(requirements.iter().any(|item| !text.contains(item)));
    for (index, requirement) in requirements.iter().enumerate() {
        if text.contains(&format!("POLICY_{index}_START")) {
            assert!(
                text.contains(requirement),
                "a constraint must never be clipped"
            );
        }
    }
    let response: Value = serde_json::from_str(
        &host
            .memory
            .query_validated(RUN, &events, r#"{"action":"tasks"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        response["tasks"].as_array().unwrap().len(),
        requirements.len()
    );
    for requirement in requirements {
        assert!(response["tasks"].as_array().unwrap().iter().any(|task| {
            task["text"] == requirement && task["evidence"][0]["quote"] == requirement
        }));
    }
}
