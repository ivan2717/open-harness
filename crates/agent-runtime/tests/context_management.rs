use agent_runtime::{ContentBlock, MessageContext, ModelRole};
#[path = "support/history.rs"]
mod history;
use history::persist_run;

#[path = "support/scripted_model.rs"]
mod scripted_model;

use scripted_model::ScriptedModel;

use std::{
    convert::Infallible,
    future::Future,
    sync::{Arc, Mutex},
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, ApproximateTokenCounter, CompactingContextManager, ContextCheckpointStore,
    ContextCompactor, ContextInput, ContextManager, ContextPolicy, ContextSummary, EventStore,
    EventStoreHistoryAccess, HistoryAccess, HistoryContext, InMemoryContextCheckpointStore,
    InMemoryEventStore, ModelContextCompactor, ModelMessage, ModelRequest, ModelResponse,
    READ_RUN_HISTORY_TOOL, SEARCH_RUN_HISTORY_TOOL, ToggleHistoryAccess, TokenCounter, ToolCall,
    ToolDefinition, ToolExecutor,
};

#[derive(Default)]
struct NoTools;

impl ToolExecutor for NoTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }

    async fn execute(&self, _call: ToolCall) -> Result<String, Self::Error> {
        unreachable!("the test model did not request a regular tool")
    }
}

#[derive(Debug, Clone)]
struct CompactionCall {
    previous_summary: Option<ContextSummary>,
    events: Vec<RunEventEnvelope>,
}

#[derive(Debug, Clone, Default)]
struct RecordingCompactor {
    calls: Arc<Mutex<Vec<CompactionCall>>>,
}

impl RecordingCompactor {
    fn calls(&self) -> Vec<CompactionCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl ContextCompactor for RecordingCompactor {
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

    fn compact(
        &self,
        previous_summary: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> impl Future<Output = Result<ContextSummary, Self::Error>> + Send {
        self.calls.lock().unwrap().push(CompactionCall {
            previous_summary,
            events,
        });
        async {
            Ok(ContextSummary {
                objective: "Continue the original task".to_owned(),
                current_state: "Older run events were compacted".to_owned(),
                next_action: "Continue from the recent events".to_owned(),
                ..ContextSummary::default()
            })
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FailingCompactor;

impl ContextCompactor for FailingCompactor {
    type Error = std::io::Error;

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
        _events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        Err(std::io::Error::other("temporary summary failure"))
    }
}

fn test_agent() -> Agent {
    Agent::new(
        "context-agent",
        "Context agent",
        "Exercises runtime context management",
        "Use the supplied context precisely.",
    )
}

fn append_context_step(run: &mut AgentRun, marker: &str) {
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: format!("{marker}-assistant-{}", "a".repeat(90)),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: format!("{marker}-call"),
        tool_name: "read_file".to_owned(),
        arguments: format!(r#"{{"path":"{marker}.txt"}}"#),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: format!("{marker}-call"),
        output: format!("{marker}-output-{}", "o".repeat(90)),
    })
    .unwrap();
}

fn compacting_manager(
    policy: ContextPolicy,
    compactor: RecordingCompactor,
    checkpoints: InMemoryContextCheckpointStore,
) -> CompactingContextManager<
    RecordingCompactor,
    InMemoryContextCheckpointStore,
    ApproximateTokenCounter,
> {
    CompactingContextManager::new(
        policy,
        compactor,
        checkpoints,
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

#[tokio::test]
async fn compacting_runtime_keeps_complete_history_below_the_threshold() {
    let agent = test_agent();
    let compactor = RecordingCompactor::default();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(3_000)
            .recent_tail_tokens(1_000)
            .max_tool_output_chars(1_000)
            .build()
            .unwrap(),
        compactor.clone(),
        InMemoryContextCheckpointStore::new(),
    );
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::input_required("Which environment?".to_owned()),
            ModelResponse::final_answer("Using staging.".to_owned()),
        ]),
        NoTools,
        manager,
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "bounded-context", "Inspect the deployment")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    runtime.submit_input(&mut run, "staging").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let requests = runtime.model().requests();
    assert_eq!(
        requests[1].messages,
        vec![
            ModelMessage::user("Inspect the deployment".to_owned()),
            ModelMessage::assistant("Which environment?".to_owned(), None),
            ModelMessage::user("staging".to_owned()),
        ]
    );
    assert!(!requests[1].messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. }))
    }));

    let stored = runtime.event_store().load(run.id()).await.unwrap().unwrap();
    assert_eq!(stored.events, run.events());
    assert!(compactor.calls().is_empty());
}

#[tokio::test]
async fn compacting_runtime_sends_a_summary_without_deleting_durable_events() {
    let agent = test_agent();
    let compactor = RecordingCompactor::default();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(500)
            .recent_tail_tokens(200)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        compactor.clone(),
        checkpoints.clone(),
    );
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::input_required(format!("Confirm the target. {}", "p".repeat(3_000))),
            ModelResponse::final_answer("Compacted safely.".to_owned()),
        ]),
        NoTools,
        manager,
        Some(3),
    );
    let original_instruction = format!("historical requirement: {}", "x".repeat(1_200));
    let mut run = runtime
        .start_run(&agent, "compacted-runtime", original_instruction.clone())
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    runtime.submit_input(&mut run, "staging").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let requests = runtime.model().requests();
    assert!(requests[1].messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. })) && {
            let text = candidate.text();
            let content = text.as_str();
            content.contains("Continue the original task")
        }
    }));
    assert!(requests[1].messages.iter().any(|message| {
        let candidate = message;
        candidate.role == ModelRole::User && candidate.context.is_none() && {
            let text = candidate.text();
            let content = text.as_str();
            content == "staging"
        }
    }));

    let checkpoint = checkpoints
        .latest(run.id().to_owned())
        .await
        .unwrap()
        .expect("the oversized request should create a checkpoint");
    assert_eq!(checkpoint.through_sequence, 4);
    assert!(!compactor.calls().is_empty());

    let stored = runtime.event_store().load(run.id()).await.unwrap().unwrap();
    assert_eq!(stored.events, run.events());
    assert!(matches!(
        &stored.events[0].event,
        RunEvent::RunStarted { instruction, .. } if instruction == &original_instruction
    ));
}

#[tokio::test]
async fn checkpoints_are_reused_and_only_newly_omitted_tail_events_are_compacted() {
    let mut run = AgentRun::new(
        "checkpoint-reuse",
        "context-agent",
        format!("initial-{}", "i".repeat(100)),
    );
    for step in 1..=8 {
        append_context_step(&mut run, &format!("step-{step}"));
    }

    let compactor = RecordingCompactor::default();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(2_500)
            .compaction_threshold(800)
            .recent_tail_tokens(250)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        compactor.clone(),
        checkpoints.clone(),
    );

    let first = manager.prepare_owned(context_input(&run)).await.unwrap();
    let first_omission = first
        .omitted_history
        .clone()
        .expect("the initial oversized history should be compacted");
    assert!(first.request.messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. })) && {
            let Some(MessageContext::Summary { checkpoint_id }) = &candidate.context else {
                unreachable!()
            };
            checkpoint_id == &first_omission.checkpoint_id
        }
    }));
    let first_compaction_calls = compactor.calls().len();
    assert!(first_compaction_calls > 0);

    let reused_input = context_input(&run);
    let reused = manager.prepare(reused_input.as_ref()).await.unwrap();
    assert_eq!(reused.omitted_history, Some(first_omission.clone()));
    assert_eq!(reused.request, first.request);
    assert_eq!(compactor.calls().len(), first_compaction_calls);

    append_context_step(&mut run, "step-9");
    append_context_step(&mut run, "step-10");
    let advanced = manager.prepare_owned(context_input(&run)).await.unwrap();
    let advanced_omission = advanced
        .omitted_history
        .expect("the enlarged tail should advance the checkpoint");
    assert!(advanced_omission.through_sequence > first_omission.through_sequence);

    let calls = compactor.calls();
    let incremental_calls = &calls[first_compaction_calls..];
    assert!(!incremental_calls.is_empty());
    assert!(incremental_calls[0].previous_summary.is_some());
    assert!(incremental_calls.iter().all(|call| !call.events.is_empty()));
    assert!(
        incremental_calls
            .iter()
            .flat_map(|call| &call.events)
            .all(|event| event.sequence > first_omission.through_sequence)
    );
    assert_eq!(
        incremental_calls
            .last()
            .unwrap()
            .events
            .last()
            .unwrap()
            .sequence,
        advanced_omission.through_sequence
    );

    let latest = checkpoints
        .latest(run.id().to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, advanced_omission.checkpoint_id);
    assert_eq!(latest.through_sequence, advanced_omission.through_sequence);
}

#[tokio::test]
async fn the_hard_limit_can_emergency_compact_the_latest_completed_step() {
    let mut run = AgentRun::new("hard-limit", "context-agent", "short instruction");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Inspect these results together.".to_owned(),
    })
    .unwrap();
    for index in 0..20 {
        run.record(RunEvent::ToolCallRequested {
            call_id: format!("latest-{index}"),
            tool_name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
    }
    for index in 0..20 {
        run.record(RunEvent::ToolCallSucceeded {
            call_id: format!("latest-{index}"),
            output: "z".repeat(90),
        })
        .unwrap();
    }

    let compactor = RecordingCompactor::default();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(1_500)
            .compaction_threshold(1_200)
            .recent_tail_tokens(300)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        compactor.clone(),
        InMemoryContextCheckpointStore::new(),
    );
    let prepared = manager.prepare_owned(context_input(&run)).await.unwrap();
    let omitted = prepared
        .omitted_history
        .expect("crossing the hard limit must compact the completed latest step");

    assert_eq!(
        omitted.through_sequence,
        run.events().last().unwrap().sequence
    );
    assert!(matches!(
        prepared.request.messages.as_slice(),
        [
            ModelMessage {
                role: ModelRole::User,
                context: None,
                ..
            },
            ModelMessage {
                context: Some(MessageContext::Summary { .. }),
                ..
            }
        ]
    ));
    assert!(compactor.calls().len() > 1);
}

#[tokio::test]
async fn hard_limit_retries_without_the_preferred_recent_tail_when_needed() {
    let run_id = format!("hard-retry-{}", "r".repeat(600));
    let mut run = AgentRun::new(run_id, "context-agent", "short instruction");
    for step in 1..=20 {
        append_context_step(&mut run, &format!("retry-step-{step}"));
    }
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(1_400)
            .compaction_threshold(1_300)
            .recent_tail_tokens(1_000)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        RecordingCompactor::default(),
        InMemoryContextCheckpointStore::new(),
    );

    let prepared = manager.prepare_owned(context_input(&run)).await.unwrap();
    let omitted = prepared.omitted_history.unwrap();

    assert_eq!(
        omitted.through_sequence,
        run.events().last().unwrap().sequence
    );
    assert!(matches!(
        prepared.request.messages.as_slice(),
        [
            ModelMessage {
                role: ModelRole::User,
                context: None,
                ..
            },
            ModelMessage {
                context: Some(MessageContext::Summary { .. }),
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn a_soft_compaction_failure_falls_back_to_the_safe_original_projection() {
    let mut run = AgentRun::new(
        "soft-fallback",
        "context-agent",
        format!("large-but-safe-{}", "x".repeat(900)),
    );
    append_context_step(&mut run, "recent");
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(500)
            .recent_tail_tokens(200)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        FailingCompactor,
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );

    let prepared = manager.prepare_owned(context_input(&run)).await.unwrap();

    assert!(!prepared.request.messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. }))
    }));
}

#[tokio::test]
async fn soft_compaction_falls_back_when_deferred_tools_push_it_over_the_hard_limit() {
    let mut run = AgentRun::new("soft-tools", "context-agent", "u".repeat(500));
    for (index, content) in ["a".repeat(100), "b".repeat(400)].into_iter().enumerate() {
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            content,
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        })
        .unwrap();
        run.record(RunEvent::ToolCallRequested {
            call_id: format!("call-{index}"),
            tool_name: "read".to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id: format!("call-{index}"),
            output: String::new(),
        })
        .unwrap();
    }
    let history = EventStoreHistoryAccess::new();
    let input = ContextInput::new(
        run.id(),
        "i".repeat(1_400),
        run.events().to_vec(),
        Vec::new(),
    )
    .with_deferred_tools(HistoryAccess::<InMemoryEventStore>::definitions(&history).to_vec());
    let original = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(2_500)
            // This fixture isolates deferred-schema overhead at an exact
            // character budget; uncertainty headroom is tested separately.
            .estimation_margin_percent(0)
            .compaction_threshold(2_500)
            .recent_tail_tokens(100)
            .max_tool_output_chars(32_000)
            .build()
            .unwrap(),
        RecordingCompactor::default(),
        InMemoryContextCheckpointStore::new(),
    )
    .prepare_owned(input.clone())
    .await
    .unwrap();
    assert!(
        ApproximateTokenCounter::new(1)
            .unwrap()
            .count_tokens(&original.request)
            <= 2_500
    );
    let compactor = RecordingCompactor::default();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(2_500)
            .compaction_threshold(1_800)
            .estimation_margin_percent(0)
            .recent_tail_tokens(100)
            .max_tool_output_chars(32_000)
            .build()
            .unwrap(),
        compactor.clone(),
        checkpoints.clone(),
    );

    let prepared = manager.prepare_owned(input).await.unwrap();

    assert!(!compactor.calls().is_empty());
    assert_eq!(prepared, original);
    assert!(
        checkpoints
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_model_compactor_requests_strict_json_without_exposing_tools() {
    let summary_json = serde_json::json!({
        "objective": "Finish the context test",
        "constraints": ["preserve exact identifiers"],
        "completed_work": [],
        "current_state": "ready",
        "decisions": [],
        "important_artifacts": [],
        "failed_attempts": [],
        "unresolved_questions": [],
        "current_plan": ["continue"],
        "next_action": "run the model"
    })
    .to_string();
    let counter = ApproximateTokenCounter::new(1).unwrap();
    let compactor =
        ModelContextCompactor::new(ScriptedModel::new(vec![ModelResponse::final_answer(
            summary_json,
        )]))
        .with_token_counter(counter);
    let run = AgentRun::new("model-compactor", "context-agent", "test summarization");

    let estimated = compactor
        .estimate_input_tokens(None, run.events(), &counter)
        .unwrap();
    let summary = compactor
        .compact(None, run.events().to_vec())
        .await
        .unwrap();

    assert_eq!(summary.objective, "Finish the context test");
    let requests = compactor.model().requests();
    let raw = counter.count_tokens(&requests[0]);
    assert_eq!(estimated, raw + (raw * 15).div_ceil(100));
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].messages.len(), 1);
    assert!(
        requests[0]
            .instructions
            .contains("Return only one JSON object")
    );
}

#[tokio::test]
async fn clipped_tail_details_remain_available_through_history_access() {
    let hidden_detail = "ONLY-IN-THE-ORIGINAL-EVENT";
    let full_output = format!(
        "{}{}{}",
        "a".repeat(2_500),
        hidden_detail,
        "b".repeat(2_500)
    );
    let mut run = AgentRun::new("clipped-tail", "context-agent", "inspect a large result");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Read the large file.".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "large-result".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: r#"{"path":"large.txt"}"#.to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "large-result".to_owned(),
        output: full_output.clone(),
    })
    .unwrap();

    let access = EventStoreHistoryAccess::new();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(12_000)
            .compaction_threshold(5_000)
            .recent_tail_tokens(500)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        RecordingCompactor::default(),
        InMemoryContextCheckpointStore::new(),
    );
    let prepared = manager
        .prepare_owned(context_input(&run).with_deferred_tools(
            HistoryAccess::<InMemoryEventStore>::definitions(&access).to_vec(),
        ))
        .await
        .unwrap();
    let omitted = prepared
        .omitted_history
        .expect("clipping must make the original event readable");
    let projected_output = prepared
        .request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                content: output,
                is_error: false,
                ..
            } => Some(output),
            _ => None,
        })
        .unwrap();
    assert!(!projected_output.contains(hidden_detail));
    assert!(
        prepared
            .request
            .tools
            .iter()
            .any(|tool| tool.name == READ_RUN_HISTORY_TOOL)
    );

    let store = InMemoryEventStore::new();
    persist_run(&store, &run).await;
    let sequence = run.events().last().unwrap().sequence;
    assert_eq!(omitted.readable_through_sequence, sequence);
    let exact = access
        .execute(
            &store,
            HistoryContext {
                run_id: run.id().to_owned(),
                through_sequence: omitted.readable_through_sequence,
                checkpoint_id: omitted.checkpoint_id,
            },
            ToolCall {
                id: "recover-detail".to_owned(),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: format!(r#"{{"from_sequence":{sequence},"to_sequence":{sequence}}}"#),
            },
        )
        .await
        .unwrap();
    assert!(exact.contains(hidden_detail));
    assert!(exact.contains(&full_output));
}

#[tokio::test]
async fn a_stale_checkpoint_is_cleared_and_rebuilt_from_changed_history() {
    let mut run = AgentRun::new(
        "fingerprinted-run",
        "context-agent",
        format!("original-prefix-{}", "x".repeat(200)),
    );
    for step in 1..=6 {
        append_context_step(&mut run, &format!("fingerprint-step-{step}"));
    }
    let compactor = RecordingCompactor::default();
    let checkpoints = InMemoryContextCheckpointStore::new();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(3_000)
            .compaction_threshold(700)
            .recent_tail_tokens(200)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        compactor.clone(),
        checkpoints.clone(),
    );
    manager.prepare_owned(context_input(&run)).await.unwrap();
    let original_checkpoint = checkpoints
        .latest(run.id().to_owned())
        .await
        .unwrap()
        .expect("the original history should be compacted");
    let original_compaction_calls = compactor.calls().len();

    let mut changed_events = run.events().to_vec();
    let RunEvent::RunStarted { instruction, .. } = &mut changed_events[0].event else {
        unreachable!()
    };
    let changed_instruction = format!("different-prefix-{}", "y".repeat(200));
    *instruction = changed_instruction.clone();
    let prepared = manager
        .prepare_owned(ContextInput::new(
            run.id(),
            "Preserve exact requirements.",
            changed_events,
            Vec::new(),
        ))
        .await
        .unwrap();

    assert!(prepared.request.messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. }))
    }));
    let rebuilt_calls = &compactor.calls()[original_compaction_calls..];
    assert!(!rebuilt_calls.is_empty());
    assert!(rebuilt_calls.iter().any(|call| {
        call.events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                RunEvent::RunStarted { instruction, .. } if instruction == &changed_instruction
            )
        })
    }));

    let rebuilt_checkpoint = checkpoints
        .latest(run.id().to_owned())
        .await
        .unwrap()
        .expect("the changed history should produce a replacement checkpoint");
    assert_ne!(
        original_checkpoint.source_fingerprint,
        rebuilt_checkpoint.source_fingerprint
    );
}

#[tokio::test]
async fn history_tools_are_deferred_until_context_actually_omits_events() {
    let history = EventStoreHistoryAccess::new();
    let history_tools = HistoryAccess::<InMemoryEventStore>::definitions(&history).to_vec();
    let compactor = RecordingCompactor::default();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(2_500)
            .compaction_threshold(800)
            .recent_tail_tokens(250)
            .max_tool_output_chars(100)
            .build()
            .unwrap(),
        compactor,
        InMemoryContextCheckpointStore::new(),
    );

    let short_run = AgentRun::new("short-context", "context-agent", "short request");
    let short = manager
        .prepare_owned(context_input(&short_run).with_deferred_tools(history_tools.clone()))
        .await
        .unwrap();
    assert!(short.omitted_history.is_none());
    assert!(short.request.tools.is_empty());

    let mut long_run = AgentRun::new(
        "long-context",
        "context-agent",
        format!("long-request-{}", "x".repeat(100)),
    );
    for step in 1..=8 {
        append_context_step(&mut long_run, &format!("long-step-{step}"));
    }
    let compacted = manager
        .prepare_owned(context_input(&long_run).with_deferred_tools(history_tools))
        .await
        .unwrap();
    assert!(compacted.omitted_history.is_some());
    assert!(
        compacted
            .request
            .tools
            .iter()
            .any(|tool| tool.name == SEARCH_RUN_HISTORY_TOOL)
    );
    assert!(
        compacted
            .request
            .tools
            .iter()
            .any(|tool| tool.name == READ_RUN_HISTORY_TOOL)
    );
}

#[tokio::test]
async fn runtime_history_tool_calls_use_the_checkpoint_and_remain_ordinary_run_events() {
    let agent = test_agent();
    let manager = compacting_manager(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(500)
            .recent_tail_tokens(200)
            .max_tool_output_chars(1_000)
            .build()
            .unwrap(),
        RecordingCompactor::default(),
        InMemoryContextCheckpointStore::new(),
    );
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::input_required(format!("Which target? {}", "p".repeat(3_000))),
            ModelResponse::tool_calls(
                "I need exact historical evidence.".to_owned(),
                vec![
                    ToolCall {
                        id: "search-history".to_owned(),
                        name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
                        arguments: r#"{"query":"runtime-history-needle"}"#.to_owned(),
                    },
                    ToolCall {
                        id: "read-history".to_owned(),
                        name: READ_RUN_HISTORY_TOOL.to_owned(),
                        arguments: r#"{"from_sequence":0,"to_sequence":0}"#.to_owned(),
                    },
                    ToolCall {
                        id: "read-past-checkpoint".to_owned(),
                        name: READ_RUN_HISTORY_TOOL.to_owned(),
                        arguments: r#"{"from_sequence":1000,"to_sequence":1000}"#.to_owned(),
                    },
                ],
            ),
            ModelResponse::final_answer("Historical evidence checked.".to_owned()),
        ]),
        NoTools,
        manager,
        Some(4),
    )
    .with_history_access(EventStoreHistoryAccess::new());
    let original_instruction = format!("runtime-history-needle {}", "h".repeat(1_200));
    let mut run = runtime
        .start_run(&agent, "history-tool-runtime", original_instruction)
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    runtime.submit_input(&mut run, "staging").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let requests = runtime.model().requests();
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == SEARCH_RUN_HISTORY_TOOL)
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == SEARCH_RUN_HISTORY_TOOL)
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == READ_RUN_HISTORY_TOOL)
    );

    let requested = run
        .events()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } => Some((call_id.as_str(), tool_name.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requested,
        vec![
            ("search-history", SEARCH_RUN_HISTORY_TOOL),
            ("read-history", READ_RUN_HISTORY_TOOL),
            ("read-past-checkpoint", READ_RUN_HISTORY_TOOL),
        ]
    );

    assert!(run.events().iter().any(|envelope| matches!(
        &envelope.event,
        RunEvent::ToolCallSucceeded { call_id, output }
            if call_id == "search-history" && output.contains("runtime-history-needle")
    )));
    assert!(run.events().iter().any(|envelope| matches!(
        &envelope.event,
        RunEvent::ToolCallSucceeded { call_id, output }
            if call_id == "read-history"
                && serde_json::from_str::<serde_json::Value>(output).unwrap()["from_sequence"] == 0
    )));
    for event in run.events() {
        if let RunEvent::ToolCallSucceeded { output, .. } = &event.event {
            assert!(output.chars().count() <= 1_000);
            serde_json::from_str::<serde_json::Value>(output).unwrap();
        }
    }
    assert!(run.events().iter().any(|envelope| matches!(
        &envelope.event,
        RunEvent::ToolCallFailed { call_id, message }
            if call_id == "read-past-checkpoint"
                && message.contains("past readable checkpoint")
    )));

    let stored = runtime.event_store().load(run.id()).await.unwrap().unwrap();
    assert_eq!(stored.events, run.events());
}

#[tokio::test]
async fn saved_history_responses_repage_without_skipping_text_when_context_budget_shrinks() {
    // Exercise both previously complete responses and previously chunked ones.
    for output_chars in [3_000, 24_000] {
        let store = InMemoryEventStore::new();
        let original_output = format!("first-{}-last", "细节".repeat(output_chars / 2));
        let mut run = AgentRun::new(
            format!("rebudget-history-{output_chars}"),
            "context-agent",
            "Recover the exact original result.",
        );
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "Read the file.".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallRequested {
            call_id: "original-read".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id: "original-read".to_owned(),
            output: original_output.clone(),
        })
        .unwrap();
        persist_run(&store, &run).await;
        let sequence = run.events().last().unwrap().sequence;
        let access = ToggleHistoryAccess::enabled(EventStoreHistoryAccess::new());
        let context = HistoryContext {
            run_id: run.id().to_owned(),
            through_sequence: sequence,
            checkpoint_id: "original-checkpoint".to_owned(),
        };
        let manager = compacting_manager(
            ContextPolicy::builder()
                .input_token_limit(1_000_000)
                .compaction_threshold(900_000)
                .recent_tail_tokens(10_000)
                .max_tool_output_chars(1_000)
                .build()
                .unwrap(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
        );
        let mut offset = 0;
        let mut reconstructed = String::new();
        let mut complete = false;
        for page in 0..100 {
            let call = ToolCall {
                id: format!("history-page-{page}"),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: format!(
                    r#"{{"from_sequence":{sequence},"to_sequence":{sequence},"offset":{offset}}}"#
                ),
            };
            let output = if page == 0 {
                // This durable response predates the smaller context budget.
                let output = access
                    .execute(&store, context.clone(), call.clone())
                    .await
                    .unwrap();
                assert!(output.chars().count() > 1_000);
                output
            } else {
                access
                    .execute_bounded(
                        &store,
                        context.clone(),
                        call.clone(),
                        manager.max_tool_output_chars(),
                    )
                    .await
                    .unwrap()
            };
            if page > 0 {
                assert!(output.chars().count() <= 1_000);
            }
            run.begin_step().unwrap();
            run.record(RunEvent::ModelResponseReceived {
                response: None,
                provider_continuation: None,
                token_usage: None,
                provider_state: None,
                content: "Recover the next page.".to_owned(),
            })
            .unwrap();
            run.record(RunEvent::ToolCallRequested {
                call_id: call.id.clone(),
                tool_name: call.name,
                arguments: call.arguments,
            })
            .unwrap();
            run.record(RunEvent::ToolCallSucceeded {
                call_id: call.id.clone(),
                output,
            })
            .unwrap();
            let input = context_input(&run);
            let prepared = if page == 0 {
                manager
                    .compact_now(input.as_ref().with_tool_output_projector(&access))
                    .await
                    .unwrap()
                    .unwrap()
            } else {
                manager
                    .prepare(input.as_ref().with_tool_output_projector(&access))
                    .await
                    .unwrap()
            };
            let projected = prepared
                .request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .find_map(|block| match block {
                    ContentBlock::ToolResult {
                        call_id,
                        content: output,
                        ..
                    } if call_id == &call.id => Some(output),
                    _ => None,
                })
                .unwrap();
            assert!(projected.chars().count() <= 1_000);
            let response: serde_json::Value = serde_json::from_str(projected).unwrap();
            assert_eq!(response["offset"], offset);
            let chunk = response["chunk"].as_str().unwrap();
            assert!(!chunk.is_empty());
            reconstructed.push_str(chunk);
            offset += chunk.chars().count();
            complete = response["complete"].as_bool().unwrap();
            if complete {
                assert!(response["next_offset"].is_null());
                break;
            }
            assert_eq!(response["next_offset"], offset);
        }

        assert!(
            complete,
            "history pagination did not finish within 100 pages"
        );
        let payload: serde_json::Value = serde_json::from_str(&reconstructed).unwrap();
        assert_eq!(payload["events"][0]["event"]["output"], original_output);
    }
}

fn context_input(run: &AgentRun) -> ContextInput {
    ContextInput::new(
        run.id(),
        "Preserve exact requirements.",
        run.events().to_vec(),
        Vec::new(),
    )
}
