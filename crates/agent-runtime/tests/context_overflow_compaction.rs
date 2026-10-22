use std::{
    convert::Infallible,
    io,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, ApproximateTokenCounter, CompactingContextManager, ContentBlock,
    ContextCheckpointStore, ContextCompactor, ContextInput, ContextManager,
    ContextMeasurementSource, ContextPolicy, ContextSummary, InMemoryContextCheckpointStore,
    MessageContext, Model, ModelContextMetadata, ModelMessage, ModelRequest, ModelResponse,
    ModelUsage, ModelUsageRecord, TokenCounter, ToolCall, ToolDefinition, ToolExecutor,
    request_usage_baseline,
};

#[derive(Default)]
struct SummaryCompactor(Mutex<Vec<Vec<RunEventEnvelope>>>);
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
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.0.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Continue inspecting records.".into(),
            ..ContextSummary::default()
        })
    }
}

struct OverflowOnce {
    requests: Mutex<Vec<ModelRequest>>,
    metadata: ModelContextMetadata,
}
impl Model for OverflowOnce {
    type Error = io::Error;
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }
    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        error.kind() == io::ErrorKind::InvalidInput
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        if requests.len() == 1 {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "provider-confirmed overflow",
            ))
        } else {
            Ok(ModelResponse::final_answer("Complete".into()))
        }
    }
}

struct Tools {
    calls: AtomicUsize,
    definitions: Vec<ToolDefinition>,
}
impl ToolExecutor for Tools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("executed".into())
    }
}

fn record_response(
    run: &mut AgentRun,
    response: ModelResponse,
    token_usage: Option<Box<ModelUsageRecord>>,
) {
    run.record(RunEvent::ModelResponseReceived {
        content: response.text(),
        response: Some(response),
        token_usage,
        provider_state: None,
        provider_continuation: None,
    })
    .unwrap();
}

fn record_tool_step(
    run: &mut AgentRun,
    id: &str,
    content: String,
    token_usage: Option<Box<ModelUsageRecord>>,
) {
    run.begin_step().unwrap();
    record_response(
        run,
        ModelResponse::tool_calls(
            content,
            vec![ToolCall {
                id: id.into(),
                name: "inspect".into(),
                arguments: "{}".into(),
            }],
        ),
        token_usage,
    );
    run.record(RunEvent::ToolCallRequested {
        call_id: id.into(),
        tool_name: "inspect".into(),
        arguments: "{}".into(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: id.into(),
        output: "inspected".into(),
    })
    .unwrap();
}

async fn assert_forced_compaction(calibrated: bool) {
    let agent = Agent::new("agent", "Agent", "", "Inspect the records.");
    let original = "Inspect the first record, preserving all user requirements.";
    let correction = "Correction: inspect only the public fields.";
    let mut run = agent.start_run("forced-overflow", original);
    record_tool_step(
        &mut run,
        "old",
        "Old internal observations. ".repeat(200),
        None,
    );
    run.begin_step().unwrap();
    record_response(
        &mut run,
        ModelResponse::input_required("Which fields?".into()),
        None,
    );
    run.record(RunEvent::InputRequested {
        prompt: "Which fields?".into(),
    })
    .unwrap();
    run.record(RunEvent::InputReceived {
        files: Vec::new(),
        content: correction.into(),
    })
    .unwrap();

    let policy = ContextPolicy::builder()
        .input_token_limit(10_000)
        .compaction_threshold(5_000)
        .compaction_target(3_000)
        .recent_tail_tokens(500)
        .build()
        .unwrap();
    let store = InMemoryContextCheckpointStore::new();
    let manager = CompactingContextManager::new(
        policy,
        SummaryCompactor::default(),
        store.clone(),
        ApproximateTokenCounter::default(),
    );
    let metadata = ModelContextMetadata::new("test", "overflow-model", 128_000).unwrap();
    let definitions = vec![ToolDefinition {
        name: "inspect".into(),
        description: "Inspect a record".into(),
        input_schema: "{}".into(),
    }];
    let pre_response_input = ContextInput::new(
        run.id(),
        &agent.instructions,
        run.events().to_vec(),
        definitions.clone(),
    )
    .with_model_metadata(metadata.clone());
    let pre_response_request = manager
        .prepare(pre_response_input.as_ref())
        .await
        .unwrap()
        .request;
    let token_usage = calibrated.then(|| {
        Box::new(ModelUsageRecord {
            usage: ModelUsage {
                input_tokens: 1,
                output_tokens: 10,
                cached_input_tokens: None,
            },
            baseline: request_usage_baseline(Some(&metadata), &pre_response_request),
        })
    });
    record_tool_step(
        &mut run,
        "recent",
        "Recent observations.".into(),
        token_usage,
    );

    let input = ContextInput::new(
        run.id(),
        &agent.instructions,
        run.events().to_vec(),
        definitions.clone(),
    )
    .with_model_metadata(metadata.clone());
    let before = manager.prepare(input.as_ref()).await.unwrap();
    let before_measurement = manager
        .measure_request(input.as_ref(), &before.request)
        .unwrap();
    assert!(before_measurement.decision_input_tokens < 5_000);
    if calibrated {
        assert_eq!(
            before_measurement.source,
            ContextMeasurementSource::ProviderCalibrated
        );
        assert!(
            before_measurement.estimated_input_tokens
                < before_measurement.raw_estimated_input_tokens
        );
    }
    assert!(
        manager.compactor().0.lock().unwrap().is_empty(),
        "ordinary preparation must not compact below the threshold"
    );
    assert!(store.latest(run.id().into()).await.unwrap().is_none());

    let runtime = AgentRuntime::new(
        OverflowOnce {
            requests: Mutex::new(Vec::new()),
            metadata,
        },
        Tools {
            calls: AtomicUsize::new(0),
            definitions,
        },
        manager,
        Some(5),
    );
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(
        runtime.tools().calls.load(Ordering::SeqCst),
        0,
        "persisted tool effects must not run again"
    );
    assert_eq!(
        run.current_step(),
        4,
        "recovery must stay inside the new step"
    );

    let checkpoint = store.latest(run.id().into()).await.unwrap().unwrap();
    assert!(checkpoint.after_tokens < checkpoint.before_tokens);
    if calibrated {
        assert!(
            checkpoint.after_tokens > before_measurement.decision_input_tokens,
            "the rewritten estimate exceeds the old calibrated baseline but still shrinks the same-counter raw request"
        );
    }
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], before.request);
    let retried = &requests[1];
    assert!(
        ApproximateTokenCounter::default().count_tokens(retried)
            < ApproximateTokenCounter::default().count_tokens(&requests[0])
    );
    assert_eq!(
        retried
            .messages
            .iter()
            .filter(|message| **message == ModelMessage::user(correction.into()))
            .count(),
        1
    );
    assert!(
        !retried
            .messages
            .contains(&ModelMessage::user(original.into()))
    );
    assert!(retried.messages.iter().any(|message| matches!(&message.context, Some(MessageContext::Summary { checkpoint_id }) if checkpoint_id == &checkpoint.id)));
    let recent_calls = retried
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolCall { id, .. } if id == "recent"))
        .count();
    let recent_results = retried.messages.iter().flat_map(|message| &message.content).filter(|block| matches!(block, ContentBlock::ToolResult { call_id, .. } if call_id == "recent")).count();
    assert_eq!(
        (recent_calls, recent_results),
        (1, 1),
        "the recent call/result pair must remain intact"
    );
    let compacted = runtime.context_manager().compactor().0.lock().unwrap();
    assert_eq!(compacted.len(), 1);
    assert!(compacted[0].iter().any(|event| matches!(&event.event, RunEvent::ToolCallRequested { call_id, .. } if call_id == "old")));
    assert!(compacted[0].iter().any(|event| matches!(&event.event, RunEvent::ToolCallSucceeded { call_id, .. } if call_id == "old")));
    assert_eq!(
        compacted[0].last().unwrap().sequence,
        checkpoint.through_sequence
    );
    assert!(
        compacted[0].iter().all(|event| event.step < 4),
        "an unsettled current request must never enter the saved checkpoint"
    );
}

#[tokio::test]
async fn confirmed_overflow_forces_durable_compaction_below_the_estimated_threshold() {
    assert_forced_compaction(false).await;
}

#[tokio::test]
async fn overflow_recovery_compares_raw_counts_when_rewriting_invalidates_a_lower_usage_baseline() {
    assert_forced_compaction(true).await;
}
