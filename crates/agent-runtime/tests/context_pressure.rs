use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContentBlock, ContextCheckpoint,
    ContextCheckpointStore, ContextCompactor, ContextInput, ContextManager, ContextPolicy,
    ContextSummary, InMemoryContextCheckpointStore, ModelMessage, TokenCounter, ToolDefinition,
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

#[derive(Clone, Default)]
struct Summarizer {
    calls: Arc<AtomicUsize>,
    fail: bool,
    started: Option<Arc<tokio::sync::Notify>>,
}
impl ContextCompactor for Summarizer {
    type Error = io::Error;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        _: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(started) = &self.started {
            started.notify_one();
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(io::Error::other("summary service unavailable"));
        }
        Ok(ContextSummary {
            objective: "Keep the historical constraints and continue.".into(),
            ..Default::default()
        })
    }
}
fn event(sequence: u64, step: u32, event: RunEvent) -> RunEventEnvelope {
    RunEventEnvelope {
        sequence,
        step,
        occurred_at: SystemTime::UNIX_EPOCH,
        event,
    }
}
fn history(output: usize, assistant: usize) -> ContextInput {
    ContextInput::new(
        "pressure",
        "system rules",
        vec![
            event(
                0,
                0,
                RunEvent::RunStarted {
                    files: Vec::new(),
                    instruction: "Inspect records.".into(),
                },
            ),
            event(
                1,
                1,
                RunEvent::ModelResponseReceived {
                    content: "a".repeat(assistant),
                    response: None,
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: None,
                },
            ),
            event(
                2,
                1,
                RunEvent::ToolCallRequested {
                    call_id: "read-1".into(),
                    tool_name: "read".into(),
                    arguments: "{}".into(),
                },
            ),
            event(
                3,
                1,
                RunEvent::ToolCallSucceeded {
                    call_id: "read-1".into(),
                    output: format!("head{}tail", "x".repeat(output)),
                    images: Vec::new(),
                },
            ),
            event(
                4,
                2,
                RunEvent::ModelResponseReceived {
                    content: "Continue with recent evidence".into(),
                    response: None,
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: None,
                },
            ),
        ],
        vec![ToolDefinition {
            name: "read".into(),
            description: "Read".into(),
            input_schema: "{}".into(),
        }],
    )
    .with_deferred_tools(vec![ToolDefinition {
        name: "history".into(),
        description: "Read original details".into(),
        input_schema: "{}".into(),
    }])
}
fn policy(trigger: usize) -> ContextPolicy {
    ContextPolicy::builder()
        .input_token_limit(30_000)
        .compaction_threshold(trigger)
        .compaction_target(trigger * 3 / 4)
        .recent_tail_tokens(300)
        .max_tool_output_chars(100)
        .estimation_margin_percent(0)
        .build()
        .unwrap()
}
fn manager(
    policy: ContextPolicy,
    summary: Summarizer,
    store: InMemoryContextCheckpointStore,
) -> CompactingContextManager<Summarizer, InMemoryContextCheckpointStore, ApproximateTokenCounter> {
    CompactingContextManager::new(
        policy,
        summary,
        store,
        ApproximateTokenCounter::new(1).unwrap(),
    )
}
fn tool_result(request: &agent_runtime::ModelRequest, id: &str) -> Option<String> {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                call_id, content, ..
            } if call_id == id => Some(content.clone()),
            _ => None,
        })
}

#[tokio::test]
async fn low_pressure_keeps_large_tool_results_verbatim() {
    let input = history(2_000, 100);
    let store = InMemoryContextCheckpointStore::new();
    let summary = Summarizer::default();
    let context = manager(policy(20_000), summary.clone(), store.clone())
        .prepare(input.as_ref())
        .await
        .unwrap();
    assert_eq!(
        tool_result(&context.request, "read-1").unwrap(),
        format!("head{}tail", "x".repeat(2_000))
    );
    assert!(context.omitted_history.is_none());
    assert_eq!(summary.calls.load(Ordering::Relaxed), 0);
    assert!(store.latest(input.run_id.clone()).await.unwrap().is_none());
}

#[tokio::test]
async fn pressure_pruning_avoids_summary_and_replays_exactly_after_restart() {
    let mut input = history(8_000, 100);
    let original = input.events.clone();
    let store = InMemoryContextCheckpointStore::new();
    let summary = Summarizer::default();
    let context = manager(policy(5_000), summary.clone(), store.clone())
        .prepare(input.as_ref())
        .await
        .unwrap();
    assert_eq!(
        tool_result(&context.request, "read-1")
            .unwrap()
            .chars()
            .count(),
        100
    );
    assert_eq!(summary.calls.load(Ordering::Relaxed), 0);
    assert!(
        context
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "history")
    );
    let checkpoint = store.latest(input.run_id.clone()).await.unwrap().unwrap();
    assert!(checkpoint.summary.is_empty());
    assert_eq!(checkpoint.summary_through_sequence(), None);
    assert_eq!(checkpoint.pruning.as_ref().unwrap().replacements.len(), 1);
    assert_eq!(input.events, original);
    let reopened = manager(policy(5_000), summary.clone(), store.clone());
    assert_eq!(reopened.prepare(input.as_ref()).await.unwrap(), context);
    input.events.push(event(
        5,
        2,
        RunEvent::ToolCallRequested {
            call_id: "read-2".into(),
            tool_name: "read".into(),
            arguments: "{}".into(),
        },
    ));
    input.events.push(event(
        6,
        2,
        RunEvent::ToolCallSucceeded {
            call_id: "read-2".into(),
            output: "new".repeat(200),
            images: Vec::new(),
        },
    ));
    let extended = reopened.prepare(input.as_ref()).await.unwrap();
    assert_eq!(
        tool_result(&extended.request, "read-2").unwrap(),
        "new".repeat(200)
    );
    assert_eq!(
        tool_result(&extended.request, "read-1"),
        tool_result(&context.request, "read-1")
    );
    assert_eq!(
        extended.omitted_history.unwrap().readable_through_sequence,
        3
    );
}

#[tokio::test]
async fn summary_failure_keeps_durable_pruning_usable() {
    let input = history(8_000, 2_000);
    let store = InMemoryContextCheckpointStore::new();
    let summary = Summarizer {
        fail: true,
        ..Default::default()
    };
    let context = manager(policy(1_000), summary.clone(), store.clone())
        .prepare(input.as_ref())
        .await
        .unwrap();
    assert_eq!(summary.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        tool_result(&context.request, "read-1")
            .unwrap()
            .chars()
            .count(),
        100
    );
    assert!(
        store
            .latest(input.run_id.clone())
            .await
            .unwrap()
            .unwrap()
            .pruning
            .is_some()
    );
    assert_eq!(
        manager(policy(1_000), summary, store)
            .prepare(input.as_ref())
            .await
            .unwrap(),
        context
    );
}

#[tokio::test]
async fn cancellation_during_summary_preserves_committed_pruning() {
    let input = history(8_000, 2_000);
    let store = InMemoryContextCheckpointStore::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let pending = manager(
        policy(1_000),
        Summarizer {
            started: Some(started.clone()),
            ..Default::default()
        },
        store.clone(),
    );
    let owned = input.clone();
    let work = tokio::spawn(async move { pending.prepare(owned.as_ref()).await });
    started.notified().await;
    work.abort();
    assert!(work.await.unwrap_err().is_cancelled());
    let checkpoint = store.latest(input.run_id.clone()).await.unwrap().unwrap();
    assert!(checkpoint.pruning.is_some());
    let resumed = manager(
        policy(1_000),
        Summarizer {
            fail: true,
            ..Default::default()
        },
        store,
    )
    .prepare(input.as_ref())
    .await
    .unwrap();
    assert_eq!(
        tool_result(&resumed.request, "read-1")
            .unwrap()
            .chars()
            .count(),
        100
    );
}

#[tokio::test]
async fn forced_overflow_can_recover_with_pruning_alone_below_the_trigger() {
    let input = history(2_000, 100);
    let summary = Summarizer::default();
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(policy(20_000), summary.clone(), store);
    let original = manager.prepare(input.as_ref()).await.unwrap();
    let recovered = manager
        .recover_overflow(input.as_ref(), &original.request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tool_result(&recovered.request, "read-1")
            .unwrap()
            .chars()
            .count(),
        100
    );
    assert_eq!(summary.calls.load(Ordering::Relaxed), 0);
    assert_eq!(manager.prepare(input.as_ref()).await.unwrap(), recovered);
}

#[tokio::test]
async fn manual_compaction_runs_below_trigger_and_reports_no_progress() {
    let input = history(0, 6_000);
    let summary = Summarizer::default();
    let manager = manager(
        policy(20_000),
        summary.clone(),
        InMemoryContextCheckpointStore::new(),
    );
    assert!(
        manager
            .prepare(input.as_ref())
            .await
            .unwrap()
            .omitted_history
            .is_none()
    );
    let compacted = manager.compact_now(input.as_ref()).await.unwrap().unwrap();
    assert_eq!(summary.calls.load(Ordering::Relaxed), 1);
    assert!(
        compacted
            .request
            .messages
            .iter()
            .any(|message| message.context.is_some())
    );
    let empty = ContextInput::new(
        "empty",
        "system",
        vec![event(
            0,
            0,
            RunEvent::RunStarted {
                files: Vec::new(),
                instruction: "short".into(),
            },
        )],
        Vec::new(),
    );
    assert!(manager.compact_now(empty.as_ref()).await.unwrap().is_none());
}

#[tokio::test]
async fn old_user_messages_are_summarized_while_latest_input_and_canonical_log_survive() {
    let mut input = history(0, 6_000);
    input.events.push(event(
        5,
        3,
        RunEvent::InputReceived {
            files: Vec::new(),
            content: "latest correction".into(),
        },
    ));
    input.events.push(event(
        6,
        4,
        RunEvent::ModelResponseReceived {
            content: "recent".into(),
            response: None,
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        },
    ));
    let original = input.events.clone();
    let store = InMemoryContextCheckpointStore::new();
    let context = manager(policy(3_000), Summarizer::default(), store.clone())
        .prepare(input.as_ref())
        .await
        .unwrap();
    assert!(
        !context
            .request
            .messages
            .contains(&ModelMessage::user("Inspect records.".into()))
    );
    assert_eq!(
        context
            .request
            .messages
            .iter()
            .filter(|message| **message == ModelMessage::user("latest correction".into()))
            .count(),
        1
    );
    assert_eq!(context.request.instructions, "system rules");
    assert!(context.request.tools.iter().any(|tool| tool.name == "read"));
    assert_eq!(input.events, original);
    // Existing summary records remain readable when the optional pruning field
    // is absent, without assigning them a pruning-only meaning.
    let checkpoint = store.latest(input.run_id.clone()).await.unwrap().unwrap();
    let mut encoded = serde_json::to_value(&checkpoint).unwrap();
    encoded.as_object_mut().unwrap().remove("pruning");
    let restored: ContextCheckpoint = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        restored.summary_through_sequence(),
        Some(checkpoint.through_sequence)
    );
    assert!(restored.pruning.is_none());
}

#[derive(Default)]
struct PrefixSummarizer(std::sync::Mutex<Vec<agent_runtime::ModelRequest>>);
impl ContextCompactor for PrefixSummarizer {
    type Error = io::Error;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        panic!("a fitting compatible prefix must use the cache-aware path")
    }
    fn estimate_projected_input_tokens(
        &self,
        _: Option<&agent_runtime::ModelContextMetadata>,
        _: &agent_runtime::ModelRequest,
    ) -> Result<Option<usize>, Self::Error> {
        Ok(Some(1))
    }
    async fn compact_projected(
        &self,
        _: Option<agent_runtime::ModelContextMetadata>,
        request: agent_runtime::ModelRequest,
    ) -> Result<Option<ContextSummary>, Self::Error> {
        self.0.lock().unwrap().push(request);
        Ok(Some(ContextSummary {
            objective: "Continue".into(),
            ..Default::default()
        }))
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        _: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        panic!("a fitting compatible prefix must use the cache-aware path")
    }
}

#[tokio::test]
async fn manager_sends_the_original_fitting_prefix_to_the_projected_compactor() {
    let input = history(0, 6_000).with_model_metadata(
        agent_runtime::ModelContextMetadata::new("test", "same-model", 30_000).unwrap(),
    );
    let original = manager(
        policy(20_000),
        Summarizer::default(),
        InMemoryContextCheckpointStore::new(),
    )
    .prepare(input.as_ref())
    .await
    .unwrap();
    let compacting = CompactingContextManager::new(
        policy(3_000),
        PrefixSummarizer::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let prepared = compacting.prepare(input.as_ref()).await.unwrap();
    assert!(prepared.omitted_history.is_some());
    let requests = compacting.compactor().0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].instructions, original.request.instructions);
    assert_eq!(requests[0].tools, original.request.tools);
    assert_eq!(
        requests[0].messages,
        original.request.messages[..original.request.messages.len() - 1]
    );
    assert!(requests[0].provider_context.is_none());
}
