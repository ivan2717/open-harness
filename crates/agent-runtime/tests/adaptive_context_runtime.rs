use agent_runtime::{ContentBlock, MessageContext, ModelRole};
use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use agent_core::agent::{Agent, AgentRun, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, ApproximateTokenCounter, CompactingContextError, CompactingContextManager,
    ContextCompactor, ContextInput, ContextManager, ContextPolicy, ContextPolicyError,
    ContextSummary, EventStoreHistoryAccess, InMemoryContextCheckpointStore,
    InMemoryNativeContextCheckpointStore, Model, ModelContextMetadata, ModelContextMetadataError,
    ModelMessage, ModelOutput, ModelRequest, ModelResponse, ModelUsage, NativeContextCompactor,
    NativeContextManager, ProviderContextIdentity, ProviderContextWindow, READ_RUN_HISTORY_TOOL,
    SEARCH_RUN_HISTORY_TOOL, TokenCounter, ToolCall, ToolDefinition, ToolExecutor,
};

fn metadata(window: usize) -> ModelContextMetadata {
    ModelContextMetadata::new("test-provider", "test-model", window).unwrap()
}

struct RecordingModel {
    metadata: ModelContextMetadata,
    outputs: Mutex<VecDeque<ModelOutput>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RecordingModel {
    fn new(metadata: ModelContextMetadata, outputs: impl IntoIterator<Item = ModelOutput>) -> Self {
        Self {
            metadata,
            outputs: Mutex::new(outputs.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Model for RecordingModel {
    type Error = Infallible;

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("the runtime must preserve usage metadata")
    }

    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self.outputs.lock().unwrap().pop_front().unwrap())
    }
}

#[derive(Default)]
struct RecordingCompactor {
    calls: AtomicUsize,
}

impl ContextCompactor for RecordingCompactor {
    type Error = Infallible;

    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        // This fixture represents a local extractor with no model input limit.
        Ok(0)
    }

    async fn compact(
        &self,
        _: Option<ContextSummary>,
        _: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ContextSummary {
            objective: "Finish inspecting the records.".to_owned(),
            ..ContextSummary::default()
        })
    }
}

struct InspectTool(Vec<ToolDefinition>);

impl InspectTool {
    fn new() -> Self {
        Self(vec![ToolDefinition {
            name: "inspect".to_owned(),
            description: "Inspect the next record".to_owned(),
            input_schema: "{}".to_owned(),
        }])
    }
}

impl ToolExecutor for InspectTool {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.0
    }

    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        Ok("The record was inspected.".to_owned())
    }
}

fn final_answer() -> ModelOutput {
    ModelResponse::final_answer("Inspection complete.".to_owned()).into()
}

fn tool_response(id: &str, content_length: usize, usage: ModelUsage) -> ModelOutput {
    ModelOutput {
        provider_state: None,
        response: ModelResponse::tool_calls(
            "r".repeat(content_length),
            vec![ToolCall {
                id: id.to_owned(),
                name: "inspect".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
        usage: Some(usage),
        provider_context: None,
        context_compacted: false,
    }
}

fn usage_policy() -> ContextPolicy {
    ContextPolicy::builder()
        .input_token_limit(10_000)
        .compaction_threshold(5_000)
        .compaction_target(3_000)
        .recent_tail_tokens(100)
        .build()
        .unwrap()
}

#[tokio::test]
async fn runtime_accepts_large_model_input_and_honors_its_output_cap() {
    let mut model_metadata = metadata(1_000_000);
    model_metadata.max_output_tokens = Some(4_096);
    let runtime = AgentRuntime::new(
        RecordingModel::new(model_metadata, [final_answer()]),
        InspectTool::new(),
        CompactingContextManager::new(
            ContextPolicy::default(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::default(),
        ),
        Some(1),
    );
    let agent = Agent::new("agent", "Agent", "", "Inspect the supplied document.");
    let document = "a".repeat(600_000);
    let mut run = runtime
        .start_run(&agent, "large-model", document.clone())
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        ApproximateTokenCounter::default().count_tokens(&requests[0]) > 128_000,
        "the request must exceed the old fallback window to exercise model adaptation"
    );
    assert_eq!(requests[0].max_output_tokens, Some(4_096));
    assert_eq!(requests[0].messages, vec![ModelMessage::user(document)]);
    assert_eq!(
        runtime
            .context_manager()
            .compactor()
            .calls
            .load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn lower_server_usage_preserves_history_that_the_local_estimate_would_compact() {
    let reported_usage = ModelUsage {
        input_tokens: 100,
        output_tokens: 100,
        cached_input_tokens: None,
    };
    let runtime = AgentRuntime::new(
        RecordingModel::new(
            metadata(128_000),
            [
                tool_response("first", 3_500, reported_usage),
                tool_response("second", 3_500, reported_usage),
                final_answer(),
            ],
        ),
        InspectTool::new(),
        CompactingContextManager::new(
            usage_policy(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::new(1).unwrap(),
        ),
        Some(3),
    );
    let agent = Agent::new("agent", "Agent", "", "Inspect the records.");
    let mut run = runtime
        .start_run(&agent, "usage-lower-than-estimate", "Inspect both records.")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        ApproximateTokenCounter::new(1)
            .unwrap()
            .count_tokens(&requests[2])
            > usage_policy().compaction_threshold(),
        "without the server observation this history would cross the trigger"
    );
    assert!(!requests[2].messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. }))
    }));
    for call_id in ["first", "second"] {
        assert_eq!(
            requests[2]
                .messages
                .iter()
                .filter(|message| message.content.iter().any(|block| matches!(block, ContentBlock::ToolResult {call_id: actual, is_error: false, ..} if actual == call_id)))
                .count(),
            1
        );
    }
    assert_eq!(
        runtime
            .context_manager()
            .compactor()
            .calls
            .load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn higher_server_usage_triggers_compaction_even_when_input_was_cached() {
    let runtime = AgentRuntime::new(
        RecordingModel::new(
            metadata(128_000),
            [
                tool_response(
                    "first",
                    1_000,
                    ModelUsage {
                        input_tokens: 100,
                        output_tokens: 1_000,
                        cached_input_tokens: None,
                    },
                ),
                tool_response(
                    "second",
                    1_000,
                    ModelUsage {
                        input_tokens: 6_500,
                        output_tokens: 1_000,
                        cached_input_tokens: Some(6_500),
                    },
                ),
                final_answer(),
            ],
        ),
        InspectTool::new(),
        CompactingContextManager::new(
            usage_policy(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::new(1).unwrap(),
        ),
        Some(3),
    );
    let agent = Agent::new("agent", "Agent", "", "Inspect the records.");
    let mut run = runtime
        .start_run(
            &agent,
            "cached-input-still-occupies-context",
            "Inspect both records.",
        )
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2].messages.iter().any(|message| {
            let candidate = message;
            matches!(&candidate.context, Some(MessageContext::Summary { .. }))
        }),
        "cached input reduces billing, not the occupied context window"
    );
    assert!(
        runtime
            .context_manager()
            .compactor()
            .calls
            .load(Ordering::SeqCst)
            > 0
    );
    assert!(requests[2].messages.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::ToolResult {call_id, is_error: false, ..} if call_id == "second"))));
}

#[tokio::test]
async fn calibrated_compaction_can_enable_history_tools_when_the_visible_history_is_short() {
    let runtime = AgentRuntime::new(
        RecordingModel::new(
            metadata(128_000),
            [
                tool_response(
                    "first",
                    1,
                    ModelUsage {
                        input_tokens: 100,
                        output_tokens: 100,
                        cached_input_tokens: None,
                    },
                ),
                tool_response(
                    "second",
                    1,
                    ModelUsage {
                        input_tokens: 6_500,
                        output_tokens: 1_000,
                        cached_input_tokens: None,
                    },
                ),
                final_answer(),
            ],
        ),
        InspectTool::new(),
        CompactingContextManager::new(
            ContextPolicy::builder()
                .input_token_limit(10_000)
                .compaction_threshold(5_000)
                .compaction_target(3_000)
                .recent_tail_tokens(1)
                .build()
                .unwrap(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::new(1).unwrap(),
        ),
        Some(3),
    )
    .with_history_access(EventStoreHistoryAccess::new());
    let agent = Agent::new("agent", "Agent", "", "Inspect the records.");
    let mut run = runtime
        .start_run(&agent, "usage-with-deferred-tools", "Inspect both records.")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[..2].iter().all(|request| request.tools.len() == 1),
        "history tools should remain deferred before any history is omitted"
    );
    assert!(requests[2].messages.iter().any(|message| {
        let candidate = message;
        matches!(&candidate.context, Some(MessageContext::Summary { .. }))
    }));
    for history_tool in [READ_RUN_HISTORY_TOOL, SEARCH_RUN_HISTORY_TOOL] {
        assert!(
            requests[2]
                .tools
                .iter()
                .any(|tool| tool.name == history_tool),
            "accepted compaction must expose the omitted history"
        );
    }
    let mut prior_visible_history = requests[1].clone();
    prior_visible_history.messages.extend([
        ModelMessage::assistant_blocks(
            vec![
                ContentBlock::Text {
                    text: "r".to_owned(),
                },
                ContentBlock::ToolCall {
                    id: "second".to_owned(),
                    name: "inspect".to_owned(),
                    arguments: "{}".to_owned(),
                },
            ],
            None,
        ),
        ModelMessage::tool_result(
            "second".to_owned(),
            "The record was inspected.".to_owned(),
            false,
        ),
    ]);
    prior_visible_history.tools = requests[2].tools.clone();
    assert!(
        ApproximateTokenCounter::new(1)
            .unwrap()
            .count_tokens(&requests[2])
            > ApproximateTokenCounter::new(1)
                .unwrap()
                .count_tokens(&prior_visible_history),
        "this fixture must require the observed token count when judging reduction"
    );
}

#[tokio::test]
async fn calibrated_user_history_can_grow_past_the_raw_protected_estimate() {
    let runtime = AgentRuntime::new(
        RecordingModel::new(
            metadata(128_000),
            [
                ModelOutput {
                    provider_state: None,
                    response: ModelResponse::input_required(
                        "Are there additional constraints?".to_owned(),
                    ),
                    usage: Some(ModelUsage {
                        input_tokens: 100,
                        output_tokens: 100,
                        cached_input_tokens: None,
                    }),
                    provider_context: None,
                    context_compacted: false,
                },
                final_answer(),
            ],
        ),
        InspectTool::new(),
        CompactingContextManager::new(
            usage_policy(),
            RecordingCompactor::default(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::new(1).unwrap(),
        ),
        Some(3),
    );
    let agent = Agent::new("agent", "Agent", "", "Inspect the records.");
    let original = "a".repeat(8_000);
    let addition = "b".repeat(3_000);
    let mut run = runtime
        .start_run(&agent, "calibrated-protected-history", original.clone())
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    runtime
        .submit_input(&mut run, addition.clone())
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let raw_tokens = ApproximateTokenCounter::new(1)
        .unwrap()
        .count_tokens(&requests[1]);
    assert!(raw_tokens > usage_policy().input_token_limit());
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| message.role == ModelRole::User && message.context.is_none())
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![original.as_str(), addition.as_str()],
        "provider calibration must never require deleting exact user requirements"
    );
    assert_eq!(
        runtime
            .context_manager()
            .compactor()
            .calls
            .load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn preparing_for_a_smaller_model_rechecks_the_window_and_rejects_invalid_metadata() {
    let manager = CompactingContextManager::new(
        ContextPolicy::default(),
        RecordingCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let run = AgentRun::new("switch-model", "agent", "a".repeat(600_000));
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    manager
        .prepare_owned(input.clone().with_model_metadata(metadata(1_000_000)))
        .await
        .unwrap();

    assert!(matches!(
        manager
            .prepare_owned(input.clone().with_model_metadata(metadata(128_000)))
            .await,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    let mut invalid_metadata = metadata(1_000_000);
    invalid_metadata.effective_context_window_percent = 0;
    assert!(matches!(
        manager
            .prepare_owned(input.with_model_metadata(invalid_metadata))
            .await,
        Err(CompactingContextError::Policy(
            ContextPolicyError::ModelMetadata(
                ModelContextMetadataError::InvalidEffectiveWindowPercent(0)
            )
        ))
    ));
    assert_eq!(manager.compactor().calls.load(Ordering::SeqCst), 0);
}

struct UnusedNativeCompactor(ProviderContextIdentity);

impl NativeContextCompactor for UnusedNativeCompactor {
    type Error = Infallible;

    fn identity(&self) -> &ProviderContextIdentity {
        &self.0
    }

    fn input_token_limit(&self) -> usize {
        1_000_000
    }

    async fn compact(
        &self,
        _: ModelRequest,
        _: usize,
    ) -> Result<ProviderContextWindow, Self::Error> {
        panic!("a large model must not compact this short request")
    }
}

#[tokio::test]
async fn native_context_preparation_also_uses_the_runtime_models_window() {
    let mut model_metadata = metadata(1_000_000);
    model_metadata.max_output_tokens = Some(4_096);
    let runtime = AgentRuntime::new(
        RecordingModel::new(model_metadata, [final_answer()]),
        InspectTool::new(),
        NativeContextManager::new(
            ContextPolicy::default(),
            UnusedNativeCompactor(ProviderContextIdentity {
                provider: "test-provider".to_owned(),
                model: "test-model".to_owned(),
                format: "items-v1".to_owned(),
            }),
            InMemoryNativeContextCheckpointStore::new(),
            ApproximateTokenCounter::default(),
        ),
        Some(1),
    );
    let agent = Agent::new("agent", "Agent", "", "Inspect the supplied document.");
    let document = "a".repeat(600_000);
    let mut run = runtime
        .start_run(&agent, "large-native-model", document.clone())
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_output_tokens, Some(4_096));
    assert_eq!(requests[0].messages, vec![ModelMessage::user(document)]);
}
