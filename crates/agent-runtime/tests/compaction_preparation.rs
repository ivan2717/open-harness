use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextError, CompactingContextManager,
    CompactionPreparation, CompactionPreparationHook, ContextCheckpointStore, ContextCompactor,
    ContextInput, ContextInputRef, ContextManager, ContextPolicy, ContextSummary,
    InMemoryContextCheckpointStore, ModelMessage, TokenCounter,
};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::SystemTime,
};

#[derive(Default)]
struct Compactor {
    ranges: Mutex<Vec<Vec<u64>>>,
}
impl ContextCompactor for Compactor {
    type Error = Infallible;
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
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.ranges
            .lock()
            .unwrap()
            .push(events.iter().map(|event| event.sequence).collect());
        Ok(ContextSummary {
            objective: "Continue the task".into(),
            ..Default::default()
        })
    }
}
struct Hook {
    ready: bool,
    content: String,
    calls: Mutex<Vec<(u64, Vec<RunEventEnvelope>)>>,
    pause: Option<Arc<tokio::sync::Notify>>,
}
impl Hook {
    fn new(ready: bool, content: &str) -> Self {
        Self {
            ready,
            content: content.into(),
            calls: Mutex::new(Vec::new()),
            pause: None,
        }
    }
}
impl CompactionPreparationHook for Hook {
    async fn before_compaction(
        &self,
        input: ContextInputRef<'_>,
        through_sequence: u64,
    ) -> CompactionPreparation {
        self.calls
            .lock()
            .unwrap()
            .push((through_sequence, input.events.to_vec()));
        if let Some(started) = &self.pause {
            started.notify_one();
            std::future::pending::<()>().await;
        }
        CompactionPreparation {
            derived_messages: vec![ModelMessage::summary(
                format!("wiki-{through_sequence}"),
                self.content.clone(),
            )],
            ready: self.ready,
        }
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
fn history(assistant: usize, tool: usize) -> ContextInput {
    let mut events = vec![event(
        0,
        0,
        RunEvent::RunStarted {
            instruction: "Keep requirements".into(),
            files: vec![],
        },
    )];
    events.push(event(1, 1, response("a".repeat(assistant))));
    if tool > 0 {
        events.push(event(
            2,
            1,
            RunEvent::ToolCallRequested {
                call_id: "read".into(),
                tool_name: "read_file".into(),
                arguments: "{}".into(),
            },
        ));
        events.push(event(
            3,
            1,
            RunEvent::ToolCallSucceeded {
                call_id: "read".into(),
                output: format!("head{}tail", "x".repeat(tool)),
            },
        ));
    }
    ContextInput::new("prepare", "rules", events, vec![])
}
fn response(content: String) -> RunEvent {
    RunEvent::ModelResponseReceived {
        content,
        response: None,
        provider_continuation: None,
        provider_state: None,
        token_usage: None,
    }
}
fn manager(
    limit: usize,
    threshold: usize,
    target: usize,
    tail: usize,
    store: InMemoryContextCheckpointStore,
) -> CompactingContextManager<Compactor, InMemoryContextCheckpointStore, ApproximateTokenCounter> {
    CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(limit)
            .compaction_threshold(threshold)
            .compaction_target(target)
            .recent_tail_tokens(tail)
            .max_tool_output_chars(100)
            .estimation_margin_percent(0)
            .build()
            .unwrap(),
        Compactor::default(),
        store,
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

#[tokio::test]
async fn no_hook_runs_below_threshold_or_for_tool_pruning_alone() {
    for input in [history(30, 0), history(30, 10_000)] {
        let store = InMemoryContextCheckpointStore::new();
        let manager = manager(30_000, 3_000, 2_000, 1, store.clone());
        let hook = Hook::new(true, "notes");
        let prepared = manager
            .prepare_with_hook(input.as_ref(), &hook)
            .await
            .unwrap();
        assert!(hook.calls.lock().unwrap().is_empty());
        assert!(manager.compactor().ranges.lock().unwrap().is_empty());
        if input.events.len() > 2 {
            assert!(prepared.omitted_history.is_some());
            assert!(
                store
                    .latest(input.run_id.clone())
                    .await
                    .unwrap()
                    .unwrap()
                    .summary
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn hook_sees_canonical_source_and_fresh_notes_replace_old_input() {
    let mut input = history(5_000, 5_000).with_derived_messages(vec![ModelMessage::summary(
        "old".into(),
        "obsolete notes".into(),
    )]);
    input
        .events
        .push(event(4, 2, response("Recent settled work".into())));
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(12_000, 4_000, 2_000, 1, store.clone());
    let hook = Hook::new(true, "Fresh requirements with source evidence");
    let prepared = manager
        .prepare_with_hook(input.as_ref(), &hook)
        .await
        .unwrap();
    let calls = hook.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 3);
    assert_eq!(calls[0].1, input.events);
    assert_eq!(
        prepared.request.messages[0].text(),
        "Fresh requirements with source evidence"
    );
    assert!(
        !prepared
            .request
            .messages
            .iter()
            .any(|message| message.text().contains("obsolete notes"))
    );
    assert!(prepared.omitted_history.is_some());
    assert_eq!(
        store
            .latest(input.run_id.clone())
            .await
            .unwrap()
            .unwrap()
            .summary_through_sequence(),
        Some(3)
    );
}

#[tokio::test]
async fn pending_preparation_defers_soft_pressure_but_not_hard_or_manual_compaction() {
    for mode in ["soft", "hard", "manual", "overflow"] {
        let input = history(5_000, 0);
        let store = InMemoryContextCheckpointStore::new();
        let manager = manager(
            if mode == "hard" { 4_000 } else { 12_000 },
            3_000,
            2_000,
            1,
            store.clone(),
        );
        let hook = Hook::new(false, "Partial notes; original evidence remains available");
        let prepared = match mode {
            "manual" => manager
                .compact_now_with_hook(input.as_ref(), &hook)
                .await
                .unwrap()
                .unwrap(),
            "overflow" => {
                let baseline = self::manager(
                    30_000,
                    29_000,
                    28_000,
                    1,
                    InMemoryContextCheckpointStore::new(),
                )
                .prepare(input.as_ref())
                .await
                .unwrap();
                manager
                    .recover_overflow_with_hook(input.as_ref(), &baseline.request, &hook)
                    .await
                    .unwrap()
                    .unwrap()
            }
            _ => manager
                .prepare_with_hook(input.as_ref(), &hook)
                .await
                .unwrap(),
        };
        assert_eq!(hook.calls.lock().unwrap().len(), 1);
        if mode == "soft" {
            assert!(prepared.omitted_history.is_none());
            assert!(manager.compactor().ranges.lock().unwrap().is_empty());
            assert!(store.latest(input.run_id).await.unwrap().is_none());
        } else {
            assert!(prepared.omitted_history.is_some(), "{mode}");
            assert_eq!(manager.compactor().ranges.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn refreshed_notes_cannot_bypass_the_protected_input_budget() {
    let input = history(5_000, 0);
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(4_000, 3_000, 2_000, 1, store.clone());
    let hook = Hook::new(true, &"m".repeat(5_000));
    assert!(matches!(
        manager.prepare_with_hook(input.as_ref(), &hook).await,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    assert!(store.latest(input.run_id).await.unwrap().is_none());
    assert!(manager.compactor().ranges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn expanding_a_summary_checks_each_new_boundary_before_consuming_it() {
    let mut input = history(4_000, 0);
    input.events.push(event(2, 2, response("b".repeat(1_500))));
    input.events.push(event(3, 3, response("c".repeat(1_500))));
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(10_000, 6_000, 500, 3_500, store.clone());
    let hook = Hook::new(true, "fresh memory");
    let prepared = manager
        .prepare_with_hook(input.as_ref(), &hook)
        .await
        .unwrap();
    let boundaries: Vec<_> = hook
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(through, _)| *through)
        .collect();
    assert!(boundaries.len() >= 2, "{boundaries:?}");
    assert!(boundaries.windows(2).all(|pair| pair[0] < pair[1]));
    let ranges = manager.compactor().ranges.lock().unwrap().clone();
    assert_eq!(
        boundaries,
        ranges
            .iter()
            .map(|range| *range.last().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        prepared.request.messages[0],
        ModelMessage::summary(
            format!("wiki-{}", boundaries.last().unwrap()),
            "fresh memory".into()
        )
    );
    assert_eq!(
        store
            .latest(input.run_id)
            .await
            .unwrap()
            .unwrap()
            .summary_through_sequence(),
        boundaries.last().copied()
    );
}

#[tokio::test]
async fn refreshed_notes_reduce_the_available_history_budget() {
    let mut input = history(4_000, 0);
    for sequence in 2..=4 {
        input.events.push(event(
            sequence,
            sequence as u32,
            response("b".repeat(1_000)),
        ));
    }
    let manager = manager(
        10_000,
        6_000,
        3_000,
        3_500,
        InMemoryContextCheckpointStore::new(),
    );
    let hook = Hook::new(true, &"m".repeat(1_800));
    let prepared = manager
        .prepare_with_hook(input.as_ref(), &hook)
        .await
        .unwrap();
    let boundaries: Vec<_> = hook
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(through, _)| *through)
        .collect();
    // The refreshed notes consume 1,800 tokens. Only one 1,000-token
    // recent step now fits; do not spend an extra summary pass discovering it.
    assert_eq!(boundaries, vec![1, 3]);
    assert!(
        ApproximateTokenCounter::new(1)
            .unwrap()
            .count_tokens(&prepared.request)
            <= 3_000
    );
    assert!(
        prepared
            .request
            .messages
            .iter()
            .any(|message| { message.text() == "b".repeat(1_000) })
    );
}

#[tokio::test]
async fn cancellation_during_preparation_never_commits_a_summary() {
    let input = history(5_000, 0);
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(4_000, 3_000, 2_000, 1, store.clone());
    let started = Arc::new(tokio::sync::Notify::new());
    let mut hook = Hook::new(true, "notes");
    hook.pause = Some(started.clone());
    let mut pending = Box::pin(manager.prepare_with_hook(input.as_ref(), &hook));
    tokio::select! {
        _ = started.notified() => {}
        result = &mut pending => panic!("preparation unexpectedly completed: {result:?}"),
    }
    drop(pending);
    assert!(manager.compactor().ranges.lock().unwrap().is_empty());
    assert!(store.latest(input.run_id).await.unwrap().is_none());
}
