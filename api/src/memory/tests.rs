use super::*;
use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, ContentBlock, ContextInput, ModelContextMetadata, ModelFinishReason,
    ModelRequest, ModelResponse, ProviderContextIdentity, ProviderResponseState, TokenCounter,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::sync::Notify;

static NEXT: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn broken_memory_storage_does_not_block_host_startup_and_can_be_repaired() {
    let dir = Directory::new();
    fs::write(dir.0.join("memories"), "occupied by a file").unwrap();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history(&"User requirements. ".repeat(30));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert!(fixture.requests().is_empty());
    assert!(memory.messages(input("run", &events).as_ref()).is_empty());
    assert_eq!(RunMemoryView::load(&host, "run", &events).status, "error");
    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    assert!(!ExtensionHost::open(&dir.0).unwrap().memory_enabled());
    fs::remove_file(dir.0.join("memories")).unwrap();
    host.set_plugin_enabled(PLUGIN_ID, true).unwrap();
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("run").unwrap().is_some());
}

mod archive;
mod long_tasks;
mod recall;
mod scheduling;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "harness-wiki-api-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Gate {
    started: Notify,
    release: Notify,
}

#[derive(Clone)]
struct Fixture {
    requests: Arc<StdMutex<Vec<ModelRequest>>>,
    mode: Arc<AtomicU8>,
    metadata: ModelContextMetadata,
    gate: Option<Arc<Gate>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            requests: Arc::default(),
            mode: Arc::default(),
            metadata: ModelContextMetadata::new("fixture", "wiki", 128_000).unwrap(),
            gate: None,
        }
    }
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Model for Fixture {
    type Error = io::Error;
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }
    async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
        self.requests.lock().unwrap().push(request.clone());
        if let Some(gate) = &self.gate {
            gate.started.notify_one();
            gate.release.notified().await;
        }
        if self.mode.load(Ordering::Relaxed) == 1 {
            return Err(io::Error::other(
                "sensitive provider failure sk-private-credential",
            ));
        }
        let payload = payload(&request);
        let sources = payload["new_source_events"].as_array().unwrap();
        let last = sources.last().unwrap();
        let last_text: String = last["text"].as_str().unwrap().chars().take(120).collect();
        let mut citations: Vec<_> = sources
            .iter()
            .map(|source| source["sequence"].clone())
            .collect();
        if self.mode.load(Ordering::Relaxed) == 2 {
            citations = vec![json!(u64::MAX)];
        }
        let text = json!({
            "overview": format!("Latest evidence: {last_text}"),
            "pages": [{ "id":"work", "title":"Work", "content":format!("{last_text} [event #{}]", last["sequence"]), "source_sequences":citations }],
        }).to_string();
        let mut response = ModelResponse::final_answer(text);
        if self.mode.load(Ordering::Relaxed) == 3 {
            response.finish_reason = ModelFinishReason::Length;
        }
        Ok(response)
    }
}

fn payload(request: &ModelRequest) -> Value {
    serde_json::from_str(&request.messages[0].text()).unwrap()
}

fn append(events: &mut Vec<RunEventEnvelope>, event: RunEvent) {
    events.push(RunEventEnvelope {
        sequence: events.len() as u64,
        step: events.len() as u32,
        occurred_at: SystemTime::UNIX_EPOCH,
        event,
    });
}

fn source_history(text: &str) -> Vec<RunEventEnvelope> {
    let mut events = Vec::new();
    append(
        &mut events,
        RunEvent::RunStarted {
            instruction: text.into(),
            files: vec![],
        },
    );
    events
}

fn user(events: &mut Vec<RunEventEnvelope>, text: &str) {
    append(
        events,
        RunEvent::InputReceived {
            content: text.into(),
            files: vec![],
        },
    );
}

fn input(run: &str, events: &[RunEventEnvelope]) -> ContextInput {
    ContextInput::new(run, "", events.to_vec(), vec![])
}

#[tokio::test]
async fn enabled_by_default_but_disabled_memory_makes_no_requests_and_keeps_saved_notes() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    assert!(host.memory_enabled());
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history(&"Original requirements. ".repeat(20));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let saved = host.memory.load("run").unwrap().unwrap();
    assert_eq!(fixture.requests().len(), 1);
    assert!(!memory.messages(input("run", &events).as_ref()).is_empty());

    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    user(&mut events, &"New requirements while disabled. ".repeat(20));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 1);
    assert!(memory.messages(input("run", &events).as_ref()).is_empty());
    assert_eq!(host.memory.load("run").unwrap(), Some(saved));
    assert!(!ExtensionHost::open(&dir.0).unwrap().memory_enabled());
}

#[tokio::test]
async fn failures_do_not_repeat_on_the_same_input_and_recover_after_new_evidence() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    fixture.mode.store(1, Ordering::Relaxed);
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history(&"Requirements. ".repeat(40));
    for reason in [
        UpdateReason::Compaction,
        UpdateReason::Response,
        UpdateReason::Progress,
    ] {
        memory.update(input("run", &events).as_ref(), reason).await;
    }
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("run").unwrap().is_none());
    assert!(memory.messages(input("run", &events).as_ref()).is_empty());
    let error = host.memory_errors.read().unwrap()["run"].clone();
    assert!(!error.contains("credential"));
    assert!(!error.contains("sensitive provider"));

    append(
        &mut events,
        RunEvent::ToolCallRequested {
            call_id: "read".into(),
            tool_name: "read_file".into(),
            arguments: "{}".into(),
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallSucceeded {
            call_id: "read".into(),
            output: "New tool evidence".repeat(30),
        },
    );
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(
        fixture.requests().len(),
        1,
        "a tool loop must not hammer a failing memory model"
    );

    fixture.mode.store(0, Ordering::Relaxed);
    user(&mut events, &"Retry after new input. ".repeat(30));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 2);
    assert!(host.memory.load("run").unwrap().is_some());
    assert!(!host.memory_errors.read().unwrap().contains_key("run"));
}

#[tokio::test]
async fn failures_retry_the_same_input_after_the_cooldown_expires() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    fixture.mode.store(1, Ordering::Relaxed);
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history(&"Keep the original requirements. ".repeat(20));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    fixture.mode.store(0, Ordering::Relaxed);
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("run").unwrap().is_none());

    memory.core.attempted.lock().await.as_mut().unwrap().started =
        Instant::now() - Duration::from_secs(31);
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 2);
    assert!(host.memory.load("run").unwrap().is_some());
    assert!(!host.memory_errors.read().unwrap().contains_key("run"));
    assert!(memory.core.attempted.lock().await.is_none());
}

#[tokio::test]
async fn failures_retry_after_configuration_changes_without_waiting_for_cooldown() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    fixture.mode.store(1, Ordering::Relaxed);
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history(&"Keep the original requirements. ".repeat(20));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 1);

    fixture.mode.store(0, Ordering::Relaxed);
    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    host.set_plugin_enabled(PLUGIN_ID, true).unwrap();
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(fixture.requests().len(), 2);
    assert!(host.memory.load("run").unwrap().is_some());
    assert!(!host.memory_errors.read().unwrap().contains_key("run"));
}

#[tokio::test]
async fn malformed_citations_and_incomplete_outputs_keep_the_last_good_revision() {
    for mode in [2, 3] {
        let dir = Directory::new();
        let host = ExtensionHost::open(&dir.0).unwrap();
        let fixture = Fixture::new();
        let memory = WikiMemory::new(host.clone(), fixture.clone());
        let mut events = source_history(&"Verified original work. ".repeat(20));
        memory
            .update(input("run", &events).as_ref(), UpdateReason::Compaction)
            .await;
        let saved = host.memory.load("run").unwrap().unwrap();
        fixture.mode.store(mode, Ordering::Relaxed);
        user(&mut events, &"New follow-up evidence. ".repeat(20));
        memory
            .update(input("run", &events).as_ref(), UpdateReason::Compaction)
            .await;
        assert_eq!(host.memory.load("run").unwrap(), Some(saved.clone()));
        assert!(
            memory.messages(input("run", &events).as_ref())[0]
                .text()
                .contains(&format!("Wiki revision {}", saved.revision))
        );
        let index: Value = serde_json::from_str(
            &host
                .memory
                .query_validated("run", &events, r#"{"action":"index"}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(index["overview"], saved.overview);
    }
}

#[tokio::test]
async fn corrections_are_incremental_session_scoped_and_bound_to_the_original_journal() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history(&"Use the old specification. ".repeat(20));
    memory
        .update(
            input("session-a", &events).as_ref(),
            UpdateReason::Compaction,
        )
        .await;
    let first = host.memory.load("session-a").unwrap().unwrap();
    assert!(
        memory
            .messages(input("session-b", &events).as_ref())
            .is_empty()
    );
    user(
        &mut events,
        &"Correction: use the revised specification. ".repeat(20),
    );
    let canonical = events.clone();
    memory
        .update(
            input("session-a", &events).as_ref(),
            UpdateReason::Compaction,
        )
        .await;
    let requests = fixture.requests();
    let latest_payload = payload(requests.last().unwrap());
    assert_eq!(
        latest_payload["new_source_events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(latest_payload["new_source_events"][0]["sequence"], 1);
    assert_eq!(
        latest_payload["editable_pages"][0]["content"],
        first.pages[0].content
    );
    let latest = host.memory.load("session-a").unwrap().unwrap();
    assert_eq!(latest.revision, first.revision + 1);
    assert!(latest.overview.contains("revised specification"));
    assert_eq!(events, canonical);

    if let RunEvent::RunStarted { instruction, .. } = &mut events[0].event {
        *instruction = "Changed canonical source".into();
    }
    assert!(
        memory
            .messages(input("session-a", &events).as_ref())
            .is_empty()
    );
    memory
        .update(
            input("session-a", &events).as_ref(),
            UpdateReason::Compaction,
        )
        .await;
    assert_eq!(fixture.requests().len(), requests.len());
    assert_eq!(
        host.memory
            .validate_source("session-a", &canonical)
            .unwrap(),
        Some(latest)
    );
}

#[tokio::test]
async fn chinese_backlog_is_budgeted_and_ingested_without_skipping_any_source() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history(&"中文历史".repeat(1_000));
    for _ in 1..12 {
        user(&mut events, &"新的证据".repeat(1_000));
    }
    let expected: BTreeSet<_> = events.iter().map(|event| event.sequence).collect();
    let mut seen = BTreeSet::new();
    let mut previous_cursor = None;
    let mut recovered = vec![String::new(); events.len()];
    for _ in 0..256 {
        let before = fixture.requests().len();
        memory
            .update(input("unicode", &events).as_ref(), UpdateReason::Compaction)
            .await;
        let requests = fixture.requests();
        assert!(
            requests.len() > before,
            "a successful earlier batch must not suppress the backlog"
        );
        for request in &requests[before..] {
            assert!(ApproximateTokenCounter::conservative().count_tokens(request) <= 32_000);
            for source in payload(request)["new_source_events"].as_array().unwrap() {
                assert!(
                    seen.insert((
                        source["sequence"].as_u64().unwrap(),
                        source["offset"].as_u64().unwrap()
                    )),
                    "source chunk was ingested twice"
                );
                let text = &mut recovered[source["sequence"].as_u64().unwrap() as usize];
                assert_eq!(source["offset"], text.chars().count());
                text.push_str(source["text"].as_str().unwrap());
            }
        }
        let state = host.memory.load("unicode").unwrap().unwrap();
        let cursor = state.source_cursor.unwrap();
        assert!(previous_cursor.is_none_or(|previous| cursor > previous));
        previous_cursor = Some(cursor);
        if cursor.sequence == events.len() as u64 {
            break;
        }
    }
    assert_eq!(
        seen.into_iter()
            .map(|(sequence, _)| sequence)
            .collect::<BTreeSet<_>>(),
        expected
    );
    for (text, event) in recovered.iter().zip(&events) {
        assert_eq!(text, &event.event.user_input().unwrap());
    }
    assert_eq!(
        host.memory
            .load("unicode")
            .unwrap()
            .unwrap()
            .through_sequence,
        11
    );
}

#[tokio::test]
async fn compilation_excludes_reasoning_and_memory_echoes_but_keeps_failed_attempts() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host, fixture.clone());
    let mut events = source_history(&format!(
        "{} sk-test-abcdefghijklmnopqrstuvwxyz",
        "Inspect the project. ".repeat(20)
    ));
    append(
        &mut events,
        RunEvent::ModelResponseReceived {
            content: "I will inspect the project".into(),
            response: Some(ModelResponse {
                content: vec![ContentBlock::Reasoning {
                    text: "PRIVATE_REASONING".into(),
                }],
                finish_reason: ModelFinishReason::Stop,
            }),
            provider_state: Some(
                ProviderResponseState::new(
                    ProviderContextIdentity {
                        provider: "fixture".into(),
                        model: "test".into(),
                        format: "opaque".into(),
                    },
                    "fixture",
                    ProviderReplayScope::Response,
                    json!({"opaque":"OPAQUE_SECRET"}),
                    20,
                )
                .unwrap(),
            ),
            provider_continuation: None,
            token_usage: None,
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallRequested {
            call_id: "command".into(),
            tool_name: "run_command".into(),
            arguments: "cargo test".into(),
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallFailed {
            call_id: "command".into(),
            message: "compiler error".into(),
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallRequested {
            call_id: "memory".into(),
            tool_name: "session_memory".into(),
            arguments: "{}".into(),
        },
    );
    append(
        &mut events,
        RunEvent::ToolCallSucceeded {
            call_id: "memory".into(),
            output: "MEMORY_ECHO".into(),
        },
    );
    for name in [
        agent_runtime::READ_RUN_HISTORY_TOOL,
        agent_runtime::SEARCH_RUN_HISTORY_TOOL,
    ] {
        append(
            &mut events,
            RunEvent::ToolCallRequested {
                call_id: name.into(),
                tool_name: name.into(),
                arguments: "{}".into(),
            },
        );
        append(
            &mut events,
            if name == agent_runtime::READ_RUN_HISTORY_TOOL {
                RunEvent::ToolCallSucceeded {
                    call_id: name.into(),
                    output: "HISTORY_ECHO".into(),
                }
            } else {
                RunEvent::ToolCallFailed {
                    call_id: name.into(),
                    message: "HISTORY_LOOKUP_ERROR".into(),
                }
            },
        );
    }
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let text = requests[0].messages[0].text();
    for secret in [
        "PRIVATE_REASONING",
        "OPAQUE_SECRET",
        "MEMORY_ECHO",
        "HISTORY_ECHO",
        "HISTORY_LOOKUP_ERROR",
        "sk-test-abcdefghijklmnopqrstuvwxyz",
    ] {
        assert!(!text.contains(secret));
    }
    let sources = payload(&requests[0])["new_source_events"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        sources
            .iter()
            .any(|source| source["kind"] == "tool_requested_not_completed")
    );
    assert!(sources.iter().any(|source| source["kind"] == "tool_failed"
        && source["text"].as_str().unwrap().contains("compiler error")));
}

#[tokio::test]
async fn disabling_during_compilation_prevents_an_in_flight_revision_from_committing() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new();
    fixture.gate = Some(gate.clone());
    let memory = Arc::new(WikiMemory::new(host.clone(), fixture.clone()));
    let events = source_history(&"Task evidence. ".repeat(30));
    let captured = input("run", &events);
    let task_memory = memory.clone();
    let task = tokio::spawn(async move {
        task_memory
            .update(captured.as_ref(), UpdateReason::Compaction)
            .await;
    });
    tokio::time::timeout(Duration::from_secs(2), gate.started.notified())
        .await
        .unwrap();
    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    gate.release.notify_one();
    task.await.unwrap();
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("run").unwrap().is_none());
    assert!(memory.messages(input("run", &events).as_ref()).is_empty());
}

#[tokio::test]
async fn stored_memory_is_not_inserted_into_prefix_bound_replay_or_small_model_windows() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let memory = WikiMemory::new(host, Fixture::new());
    let mut events = source_history(&"Existing task evidence. ".repeat(20));
    memory
        .update(input("run", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert!(!memory.messages(input("run", &events).as_ref()).is_empty());
    let small_input = input("run", &events)
        .with_model_metadata(ModelContextMetadata::new("fixture", "small", 8_192).unwrap());
    assert!(memory.messages(small_input.as_ref()).is_empty());
    append(
        &mut events,
        RunEvent::ModelResponseReceived {
            content: "Current response".into(),
            response: None,
            provider_state: Some(
                ProviderResponseState::new(
                    ProviderContextIdentity {
                        provider: "fixture".into(),
                        model: "bound".into(),
                        format: "opaque".into(),
                    },
                    "fixture",
                    ProviderReplayScope::Prefix,
                    json!({"opaque":"bound-state"}),
                    20,
                )
                .unwrap(),
            ),
            provider_continuation: None,
            token_usage: None,
        },
    );
    let canonical = events.clone();
    assert!(memory.messages(input("run", &events).as_ref()).is_empty());
    assert_eq!(events, canonical);
}

#[tokio::test]
async fn a_large_chinese_event_fits_a_small_compiler_window_and_does_not_block_later_events() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let mut fixture = Fixture::new();
    fixture.metadata = ModelContextMetadata::new("fixture", "small", 16_384).unwrap();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history(&"很长的中文资料".repeat(1_000));
    user(&mut events, &"后续纠正信息".repeat(1_000));

    memory
        .update(input("small", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(
        host.memory.load("small").unwrap().unwrap().through_sequence,
        0
    );
    for _ in 0..128 {
        memory
            .update(input("small", &events).as_ref(), UpdateReason::Compaction)
            .await;
        if host
            .memory
            .load("small")
            .unwrap()
            .unwrap()
            .source_cursor
            .unwrap()
            .sequence
            == 2
        {
            break;
        }
    }
    assert_eq!(
        host.memory
            .load("small")
            .unwrap()
            .unwrap()
            .source_cursor
            .unwrap()
            .sequence,
        2
    );

    let requests = fixture.requests();
    let mut recovered = [String::new(), String::new()];
    for request in &requests {
        let input_tokens = ApproximateTokenCounter::conservative().count_tokens(request);
        assert!(input_tokens + request.max_output_tokens.unwrap() + 163 <= 16_384);
        let payload = payload(request);
        for source in payload["new_source_events"].as_array().unwrap() {
            let sequence = source["sequence"].as_u64().unwrap() as usize;
            assert_eq!(source["offset"], recovered[sequence].chars().count());
            recovered[sequence].push_str(source["text"].as_str().unwrap());
        }
    }
    assert_eq!(recovered[0], events[0].event.user_input().unwrap());
    assert_eq!(recovered[1], events[1].event.user_input().unwrap());
}
