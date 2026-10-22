use agent_runtime::{ContentBlock, ModelRole};
#[path = "support/history.rs"]
mod history;

use std::{
    collections::BTreeSet,
    convert::Infallible,
    sync::{Arc, Mutex},
};

use agent_core::agent::{AgentRun, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextError, CompactingContextManager,
    ContextCheckpointStore, ContextCompactor, ContextInput, ContextManager, ContextPolicy,
    ContextSummary, EventStore, EventStoreHistoryAccess, HistoryAccess, HistoryContext,
    InMemoryContextCheckpointStore, InMemoryEventStore, ModelMessage, ModelRequest,
    READ_RUN_HISTORY_TOOL, TokenCounter, ToolCall,
};
use history::persist_run;

const INSTRUCTIONS: &str = "Follow the user's requirements and inspect evidence before acting.";

/// Deliberately discards every user constraint. These tests verify retention by
/// the context manager, not the semantic quality of a model-generated summary.
#[derive(Debug, Clone, Default)]
struct ForgetfulCompactor {
    calls: Arc<Mutex<Vec<Vec<RunEventEnvelope>>>>,
}

impl ContextCompactor for ForgetfulCompactor {
    type Error = Infallible;

    fn estimate_input_tokens(
        &self,
        previous_summary: Option<&ContextSummary>,
        events: &[RunEventEnvelope],
        token_counter: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(token_counter.count_tokens(&ModelRequest {
            instructions: String::new(),
            messages: vec![ModelMessage::user(
                serde_json::to_string(&(previous_summary, events)).unwrap(),
            )],
            tools: Vec::new(),
            max_output_tokens: None,
            provider_context: None,
        }))
    }

    async fn compact(
        &self,
        _previous_summary: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.calls.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Continue investigating.".to_owned(),
            current_state: "Earlier work was summarized.".to_owned(),
            next_action: "Inspect the remaining evidence.".to_owned(),
            ..ContextSummary::default()
        })
    }
}

fn append_tool_step(run: &mut AgentRun, label: &str) {
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: format!("Inspect both files for {label}."),
    })
    .unwrap();
    for index in 0..2 {
        run.record(RunEvent::ToolCallRequested {
            call_id: format!("{label}-{index}"),
            tool_name: "read_file".to_owned(),
            arguments: format!(r#"{{"path":"{label}-{index}.txt"}}"#),
        })
        .unwrap();
    }
    // Reverse completion order checks that projection preserves matching IDs
    // rather than relying on call/result adjacency.
    for index in (0..2).rev() {
        run.record(RunEvent::ToolCallSucceeded {
            call_id: format!("{label}-{index}"),
            output: format!("evidence-{label}-{index}:{}", "z".repeat(550)),
        })
        .unwrap();
    }
}

fn append_correction(run: &mut AgentRun, correction: &str) {
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Confirm the current requirements.".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::InputRequested {
        prompt: "Are there additional constraints?".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::InputReceived {
        files: Vec::new(),
        content: correction.to_owned(),
    })
    .unwrap();
}

fn user_messages(request: &ModelRequest) -> Vec<&str> {
    request
        .messages
        .iter()
        .filter(|message| message.role == ModelRole::User && message.context.is_none())
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_complete_tool_groups(request: &ModelRequest) {
    let mut pending = BTreeSet::new();
    for block in request.messages.iter().flat_map(|message| &message.content) {
        match block {
            ContentBlock::ToolCall { id: call_id, .. } => {
                assert!(pending.insert(call_id));
            }
            ContentBlock::ToolResult { call_id, .. } => {
                assert!(
                    pending.remove(call_id),
                    "a projected result must follow its tool call"
                );
            }
            _ => {}
        }
    }
    assert!(pending.is_empty(), "projection must retain complete steps");
}

#[tokio::test]
async fn opt_in_repeated_compaction_preserves_all_user_inputs_and_recoverable_evidence() {
    let initial = "Refactor the parser; do not change the public API or delete fixtures.";
    let corrections = [
        "Correction: use the existing parser API; keep identifiers exactly as written.",
        "Also preserve Unicode input and run the parser regression suite before finishing.",
        "Use staging only. Completion requires reporting every failing regression.",
    ];
    let mut run = AgentRun::new("retention-long-run", "context-agent", initial);
    let compactor = ForgetfulCompactor::default();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let counter = ApproximateTokenCounter::new(1).unwrap();
    let policy = ContextPolicy::builder()
        .preserve_all_user_messages(true)
        .input_token_limit(16_000)
        .compaction_threshold(5_000)
        .compaction_target(3_500)
        .recent_tail_tokens(1_500)
        .max_tool_output_chars(600)
        .build()
        .unwrap();
    let manager =
        CompactingContextManager::new(policy, compactor.clone(), checkpoints.clone(), counter);
    let access = EventStoreHistoryAccess::new();
    let history_tools = HistoryAccess::<InMemoryEventStore>::definitions(&access).to_vec();
    let mut expected_users = vec![initial];
    let mut previous_boundary = None;
    let mut latest_omission = None;

    for round in 0..4 {
        if round > 0 {
            append_correction(&mut run, corrections[round - 1]);
            expected_users.push(corrections[round - 1]);
        }
        for step in 0..6 {
            append_tool_step(&mut run, &format!("round-{round}-step-{step}"));
        }
        let original_events = run.events().to_vec();
        let prepared = manager
            .prepare_owned(
                ContextInput::new(run.id(), INSTRUCTIONS, original_events.clone(), Vec::new())
                    .with_deferred_tools(history_tools.clone()),
            )
            .await
            .unwrap();
        let omitted = prepared
            .omitted_history
            .expect("each round adds enough evidence to require a new checkpoint");

        assert_eq!(prepared.request.instructions, INSTRUCTIONS);
        assert_eq!(user_messages(&prepared.request), expected_users);
        assert_complete_tool_groups(&prepared.request);
        assert!(counter.count_tokens(&prepared.request) <= policy.compaction_target());
        assert!(previous_boundary.is_none_or(|previous| omitted.through_sequence > previous));
        assert!(
            prepared
                .request
                .tools
                .iter()
                .any(|tool| tool.name == READ_RUN_HISTORY_TOOL)
        );
        assert_eq!(
            run.events(),
            original_events,
            "compaction must not mutate source history"
        );

        let checkpoint = checkpoints
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .unwrap();
        assert!(checkpoint.summary.constraints.is_empty());
        assert_eq!(checkpoint.through_sequence, omitted.through_sequence);
        let boundary = original_events
            .iter()
            .position(|event| event.sequence == omitted.through_sequence)
            .unwrap();
        if let Some(next) = original_events.get(boundary + 1) {
            assert_ne!(original_events[boundary].step, next.step);
        }
        previous_boundary = Some(omitted.through_sequence);
        latest_omission = Some(omitted);
    }

    assert!(compactor.calls.lock().unwrap().len() >= 4);
    let store = InMemoryEventStore::new();
    persist_run(&store, &run).await;
    assert_eq!(
        store.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
    let earliest_evidence = run
        .events()
        .iter()
        .find(|event| matches!(event.event, RunEvent::ToolCallSucceeded { .. }))
        .unwrap();
    let omitted = latest_omission.unwrap();
    assert!(earliest_evidence.sequence <= omitted.through_sequence);
    let recovered = access
        .execute(
            &store,
            HistoryContext {
                run_id: run.id().to_owned(),
                through_sequence: omitted.readable_through_sequence,
                checkpoint_id: omitted.checkpoint_id,
            },
            ToolCall {
                id: "recover-original-evidence".to_owned(),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: format!(
                    r#"{{"from_sequence":{},"to_sequence":{}}}"#,
                    earliest_evidence.sequence, earliest_evidence.sequence
                ),
            },
        )
        .await
        .unwrap();
    let RunEvent::ToolCallSucceeded { output, .. } = &earliest_evidence.event else {
        unreachable!();
    };
    assert!(recovered.contains(output));
}

#[tokio::test]
async fn a_new_user_correction_is_not_duplicated_when_a_checkpoint_is_reused() {
    let initial = "Keep the exported function names unchanged.";
    let latest = "The final response must include the exact validation command.";
    let mut run = AgentRun::new("retention-reuse", "context-agent", initial);
    for step in 0..6 {
        append_tool_step(&mut run, &format!("before-{step}"));
    }
    let compactor = ForgetfulCompactor::default();
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(16_000)
            .compaction_threshold(5_000)
            .recent_tail_tokens(1_500)
            .max_tool_output_chars(600)
            .build()
            .unwrap(),
        compactor.clone(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let first = manager
        .prepare_owned(ContextInput::new(
            run.id(),
            INSTRUCTIONS,
            run.events().to_vec(),
            Vec::new(),
        ))
        .await
        .unwrap();
    assert!(first.omitted_history.is_some());
    let calls_after_first = compactor.calls.lock().unwrap().len();
    append_correction(&mut run, latest);

    for _ in 0..2 {
        let prepared = manager
            .prepare_owned(ContextInput::new(
                run.id(),
                INSTRUCTIONS,
                run.events().to_vec(),
                Vec::new(),
            ))
            .await
            .unwrap();
        assert_eq!(user_messages(&prepared.request), vec![latest]);
        assert_eq!(prepared.omitted_history, first.omitted_history);
        assert_eq!(compactor.calls.lock().unwrap().len(), calls_after_first);
    }
}

#[tokio::test]
async fn protected_user_input_that_exceeds_the_budget_is_never_silently_summarized_away() {
    let original = format!(
        "Do not omit this exact user requirement: {}",
        "原文".repeat(2_500)
    );
    let mut run = AgentRun::new("retention-too-large", "context-agent", original.clone());
    append_tool_step(&mut run, "completed");
    let original_events = run.events().to_vec();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let compactor = ForgetfulCompactor::default();
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(3_000)
            .recent_tail_tokens(500)
            .max_tool_output_chars(600)
            .build()
            .unwrap(),
        compactor.clone(),
        checkpoints.clone(),
        ApproximateTokenCounter::new(1).unwrap(),
    );

    let result = manager
        .prepare_owned(ContextInput::new(
            run.id(),
            INSTRUCTIONS,
            original_events.clone(),
            Vec::new(),
        ))
        .await;

    assert!(matches!(
        result,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    assert!(compactor.calls.lock().unwrap().is_empty());
    assert!(
        checkpoints
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(run.events(), original_events);
    assert_eq!(run.instruction(), original);
}

#[tokio::test]
async fn compaction_retains_latest_file_and_keeps_old_snapshot_searchable() {
    use agent_runtime::{SEARCH_RUN_HISTORY_TOOL, TextFileReference, format_user_input};
    let old = TextFileReference {
        name: "earlier.txt".into(),
        path: None,
        text: format!("archive-needle\n\"original\" {}", "evidence ".repeat(900)),
    };
    let latest = TextFileReference {
        name: "latest.txt".into(),
        path: None,
        text: "Keep this current file verbatim.".into(),
    };
    let mut run = AgentRun::start_with_files(
        "retention-files",
        "context-agent",
        "Compare the records",
        vec![old.clone()],
    );
    for step in 0..4 {
        append_tool_step(&mut run, &format!("before-files-{step}"));
    }
    run.record(RunEvent::RunCancelled).unwrap();
    run.record(RunEvent::RunResumed {
        content: String::new(),
        files: vec![latest.clone()],
        interrupted_calls: Vec::new(),
    })
    .unwrap();
    let original_events = run.events().to_vec();
    let compactor = ForgetfulCompactor::default();
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(20_000)
            .compaction_threshold(5_000)
            .compaction_target(3_500)
            .recent_tail_tokens(1_000)
            .build()
            .unwrap(),
        compactor.clone(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let prepared = manager
        .prepare_owned(ContextInput::new(
            run.id(),
            INSTRUCTIONS,
            original_events.clone(),
            Vec::new(),
        ))
        .await
        .unwrap();
    let omitted = prepared.omitted_history.unwrap();
    assert!(user_messages(&prepared.request).contains(&format_user_input("", &[latest]).as_str()));
    assert!(compactor.calls.lock().unwrap().iter().flatten().any(|event| matches!(&event.event, RunEvent::RunStarted { files, .. } if files == &vec![old.clone()])));
    assert_eq!(run.events(), original_events);
    let store = InMemoryEventStore::new();
    persist_run(&store, &run).await;
    let access = EventStoreHistoryAccess::new();
    let result = access
        .execute(
            &store,
            HistoryContext {
                run_id: run.id().into(),
                through_sequence: omitted.through_sequence,
                checkpoint_id: omitted.checkpoint_id,
            },
            ToolCall {
                id: "search-file".into(),
                name: SEARCH_RUN_HISTORY_TOOL.into(),
                arguments: serde_json::json!({"query":"archive-needle\n\"original\""}).to_string(),
            },
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["total_matches"], 1);
    assert_eq!(result["hits"][0]["sequence"], 0);
}
