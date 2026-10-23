use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{Agent, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextError, CompactingContextManager, ContextCompactor,
    ContextInput, ContextManager, ContextMeasurementSource, ContextPolicy, ContextSummary,
    InMemoryContextCheckpointStore, ModelResponse, TokenCounter, ToolCall, ToolDefinition,
};

#[derive(Default)]
struct SummaryCompactor(Mutex<usize>);
impl ContextCompactor for SummaryCompactor {
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
        _: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        *self.0.lock().unwrap() += 1;
        Ok(ContextSummary {
            objective: "Continue the task.".into(),
            ..Default::default()
        })
    }
}

fn policy() -> ContextPolicy {
    ContextPolicy::builder()
        .input_token_limit(10_000)
        .output_token_reserve(100)
        .compaction_threshold(8_000)
        .compaction_target(3_000)
        .recent_tail_tokens(300)
        .build()
        .unwrap()
}

fn history(old_size: usize) -> ContextInput {
    let agent = Agent::new("agent", "Agent", "", "Inspect records.");
    let mut run = agent.start_run("local-estimate", "Keep the final result concise.");
    for (id, size) in [("old", old_size), ("recent", 1_000)] {
        run.begin_step().unwrap();
        let response = ModelResponse::tool_calls(
            "Inspecting.".into(),
            vec![ToolCall {
                id: id.into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        );
        run.record(RunEvent::ModelResponseReceived {
            content: response.text(),
            response: Some(response),
            token_usage: None,
            provider_state: None,
            provider_continuation: None,
        })
        .unwrap();
        run.record(RunEvent::ToolCallRequested {
            call_id: id.into(),
            tool_name: "read".into(),
            arguments: "{}".into(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id: id.into(),
            output: "x".repeat(size),
            images: Vec::new(),
        })
        .unwrap();
    }
    ContextInput::new(
        run.id(),
        "Inspect records.",
        run.events().to_vec(),
        vec![ToolDefinition {
            name: "read".into(),
            description: "Read a record".into(),
            input_schema: "{}".into(),
        }],
    )
}

#[tokio::test]
async fn local_estimate_below_threshold_keeps_history_and_output_reserve() {
    let manager = CompactingContextManager::new(
        policy(),
        SummaryCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let input = history(24_000);
    let prepared = manager.prepare(input.as_ref()).await.unwrap();
    let measurement = manager
        .measure_request(input.as_ref(), &prepared.request)
        .unwrap();
    assert_eq!(measurement.source, ContextMeasurementSource::Estimated);
    assert_eq!(measurement.measured_input_tokens, None);
    assert!(measurement.decision_input_tokens > measurement.raw_estimated_input_tokens);
    assert!(measurement.decision_input_tokens < policy().compaction_threshold());
    assert_eq!(*manager.compactor().0.lock().unwrap(), 0);
    assert!(prepared.omitted_history.is_none());
    assert_eq!(prepared.request.max_output_tokens, Some(100));
}

#[tokio::test]
async fn uncertainty_margin_can_trigger_compaction_before_the_raw_estimate() {
    let input = history(28_000);
    let raw_manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(10_000)
            .output_token_reserve(100)
            .compaction_threshold(8_000)
            .compaction_target(3_000)
            .recent_tail_tokens(300)
            .estimation_margin_percent(0)
            .build()
            .unwrap(),
        SummaryCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let original = raw_manager.prepare(input.as_ref()).await.unwrap();
    assert!(original.omitted_history.is_none());
    let raw = raw_manager.token_counter().count_tokens(&original.request);
    assert!(raw < policy().compaction_threshold());
    assert!(raw + (raw * 15).div_ceil(100) > policy().compaction_threshold());

    let manager = CompactingContextManager::new(
        policy(),
        SummaryCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let prepared = manager.prepare(input.as_ref()).await.unwrap();
    assert!(prepared.omitted_history.is_some());
    assert!(*manager.compactor().0.lock().unwrap() > 0);
    let measurement = manager
        .measure_request(input.as_ref(), &prepared.request)
        .unwrap();
    assert_eq!(measurement.source, ContextMeasurementSource::Estimated);
    assert!(measurement.decision_input_tokens <= policy().input_token_limit());
    assert!(measurement.decision_input_tokens < raw);
}

#[tokio::test]
async fn oversized_protected_input_is_rejected_before_summarizing() {
    let manager = CompactingContextManager::new(
        policy(),
        SummaryCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let mut input = history(24_000);
    input.instructions = "x".repeat(50_000);
    assert!(matches!(
        manager.prepare(input.as_ref()).await,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    assert_eq!(*manager.compactor().0.lock().unwrap(), 0);
}
