use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::AgentRun;
use agent_runtime::{
    ApproximateTokenCounter, ContextCompactor, ContextPolicy, ContextPolicyError, ContextSummary,
    Model, ModelContextBudget, ModelContextBudgetError, ModelContextCompactor,
    ModelContextCompactorError, ModelContextMetadata, ModelMessage, ModelRequest, ModelResponse,
    ProviderContextIdentity, ProviderContextWindow, TokenCounter,
};

#[test]
fn budgets_reserve_output_and_margin_before_accepting_a_trigger() {
    let policy = ContextPolicy::builder()
        .model_context_window(10_000)
        .output_token_reserve(2_000)
        .safety_margin(1_000)
        .compaction_threshold(6_000)
        .compaction_target(3_000)
        .recent_tail_tokens(1_000)
        .build()
        .unwrap();
    assert_eq!(policy.input_token_limit(), 7_000);
    assert_eq!(policy.output_token_limit(), Some(2_000));
    assert_eq!(policy.compaction_target(), 3_000);
    assert!(matches!(
        ContextPolicy::builder()
            .model_budget(policy.model_budget())
            .compaction_threshold(8_000)
            .recent_tail_tokens(1_000)
            .build(),
        Err(ContextPolicyError::ThresholdExceedsInputLimit {
            input_token_limit: 7_000,
            ..
        })
    ));
    assert!(matches!(
        ModelContextBudget::new(usize::MAX, usize::MAX, 1),
        Err(ModelContextBudgetError::NoInputCapacity { .. })
    ));
}

#[test]
fn a_custom_legacy_limit_derives_a_valid_lower_target_without_hidden_reserves() {
    let policy = ContextPolicy::builder()
        .input_token_limit(1_000)
        .compaction_threshold(800)
        .recent_tail_tokens(100)
        .build()
        .unwrap();
    assert_eq!(policy.input_token_limit(), 1_000);
    assert_eq!(policy.output_token_limit(), None);
    assert_eq!(policy.compaction_target(), 600);
    assert!(matches!(
        ContextPolicy::builder()
            .compaction_threshold(96_000)
            .compaction_target(96_000)
            .build(),
        Err(ContextPolicyError::InvalidCompactionTarget { .. })
    ));
}

fn request(content: &str) -> ModelRequest {
    ModelRequest {
        instructions: String::new(),
        messages: vec![ModelMessage::user(content.to_owned())],
        tools: Vec::new(),
        max_output_tokens: None,
        provider_context: None,
    }
}

#[test]
fn fallback_count_handles_unicode_framing_and_native_cost_without_reading_ciphertext() {
    let counter = ApproximateTokenCounter::default();
    let mut request = request("约束🔒");
    let plain_count = counter.count_tokens(&request);
    assert!(plain_count >= "约束🔒".len());
    request.provider_context = Some(ProviderContextWindow {
        prefix_fingerprint: None,
        identity: ProviderContextIdentity {
            provider: "test".to_owned(),
            model: "test-model".to_owned(),
            format: "v1".to_owned(),
        },
        items: vec![serde_json::json!({"encrypted_content": "X".repeat(100_000)})],
        input_tokens: 47,
    });
    assert_eq!(counter.count_tokens(&request), plain_count + 47);
    assert_eq!(
        ApproximateTokenCounter::new(1)
            .unwrap()
            .count_tokens(&request),
        "约束🔒".chars().count() + 47
    );
}

struct RecordingModel {
    requests: Mutex<Vec<ModelRequest>>,
    answer: String,
    metadata: Option<ModelContextMetadata>,
}

impl RecordingModel {
    fn new(summary: ContextSummary) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            answer: serde_json::to_string(&summary).unwrap(),
            metadata: None,
        }
    }
}

impl Model for RecordingModel {
    type Error = Infallible;

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        self.metadata.as_ref()
    }

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(ModelResponse::final_answer(self.answer.clone()))
    }
}

#[tokio::test]
async fn summary_model_resolves_its_own_metadata_and_caps_explicit_overrides() {
    let mut model = RecordingModel::new(ContextSummary {
        objective: "Continue inspecting".to_owned(),
        ..ContextSummary::default()
    });
    let mut metadata = ModelContextMetadata::new("test", "summary", 32_000).unwrap();
    metadata.max_context_window = Some(64_000);
    metadata.max_output_tokens = Some(2_000);
    model.metadata = Some(metadata);
    let compactor = ModelContextCompactor::new(model);
    assert_eq!(
        compactor.resolved_budget().unwrap().context_window(),
        32_000
    );
    assert_eq!(compactor.input_token_limit(), Some(27_952));

    let compactor =
        compactor.with_budget(ModelContextBudget::new(1_000_000, 8_192, 2_048).unwrap());
    assert_eq!(
        compactor.resolved_budget().unwrap().context_window(),
        64_000
    );
    assert_eq!(compactor.input_token_limit(), Some(59_952));
    let run = AgentRun::new("summary-metadata", "agent", "Summarize");
    compactor
        .compact(None, run.events().to_vec())
        .await
        .unwrap();
    assert_eq!(
        compactor.model().requests.lock().unwrap()[0].max_output_tokens,
        Some(2_000)
    );
}

#[tokio::test]
async fn invalid_summary_model_metadata_fails_before_a_model_call() {
    let mut model = RecordingModel::new(ContextSummary::default());
    let mut metadata = ModelContextMetadata::new("test", "summary", 32_000).unwrap();
    metadata.effective_context_window_percent = 0;
    model.metadata = Some(metadata);
    let compactor = ModelContextCompactor::new(model);
    let run = AgentRun::new("bad-summary-metadata", "agent", "Summarize");
    assert!(matches!(
        compactor.compact(None, run.events().to_vec()).await,
        Err(ModelContextCompactorError::Policy(_))
    ));
    assert!(compactor.model().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn summary_model_uses_its_own_counter_input_budget_and_output_cap() {
    let own_counter = ApproximateTokenCounter::new(1).unwrap();
    let main_counter = ApproximateTokenCounter::new(100).unwrap();
    let compactor = ModelContextCompactor::new(RecordingModel::new(ContextSummary {
        objective: "Preserve the task".to_owned(),
        ..ContextSummary::default()
    }))
    .with_token_counter(own_counter)
    .with_budget(ModelContextBudget::new(10_000, 500, 1_000).unwrap());
    let run = AgentRun::new("budget", "agent", "Keep exact constraints");
    let estimate = compactor
        .estimate_input_tokens(None, run.events(), &main_counter)
        .unwrap();
    compactor
        .compact(None, run.events().to_vec())
        .await
        .unwrap();
    let requests = compactor.model().requests.lock().unwrap();
    let raw = own_counter.count_tokens(&requests[0]);
    assert_eq!(estimate, raw + (raw * 15).div_ceil(100));
    assert!(estimate > main_counter.count_tokens(&requests[0]));
    assert_eq!(compactor.input_token_limit(), Some(8_500));
    assert_eq!(requests[0].max_output_tokens, Some(500));
    assert!(requests[0].provider_context.is_none());
}

#[tokio::test]
async fn an_oversized_summary_input_is_rejected_before_calling_the_model() {
    let compactor = ModelContextCompactor::new(RecordingModel::new(ContextSummary::default()))
        .with_budget(ModelContextBudget::new(500, 100, 0).unwrap());
    let run = AgentRun::new("large", "agent", "x".repeat(2_000));
    assert!(matches!(
        compactor.compact(None, run.events().to_vec()).await,
        Err(ModelContextCompactorError::InputTokenLimitExceeded {
            input_token_limit: 400,
            ..
        })
    ));
    assert!(compactor.model().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_provider_that_ignores_the_summary_output_limit_is_rejected() {
    let compactor = ModelContextCompactor::new(RecordingModel::new(ContextSummary {
        objective: "x".repeat(1_000),
        ..ContextSummary::default()
    }))
    .with_token_counter(ApproximateTokenCounter::new(1).unwrap())
    .with_budget(ModelContextBudget::new(10_000, 500, 0).unwrap());
    let run = AgentRun::new("output-limit", "agent", "Summarize");
    assert!(matches!(
        compactor.compact(None, run.events().to_vec()).await,
        Err(ModelContextCompactorError::OutputTokenLimitExceeded {
            output_token_limit: 500,
            ..
        })
    ));
}

#[derive(Default)]
struct SmallWindowCompactor {
    batch_lengths: Mutex<Vec<usize>>,
    summary_text: Option<String>,
}

impl ContextCompactor for SmallWindowCompactor {
    type Error = Infallible;

    fn input_token_limit(&self) -> Option<usize> {
        Some(300)
    }

    fn estimate_input_tokens(
        &self,
        _previous_summary: Option<&ContextSummary>,
        events: &[agent_core::agent::RunEventEnvelope],
        _token_counter: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(events.len() * 100)
    }

    async fn compact(
        &self,
        _previous_summary: Option<ContextSummary>,
        events: Vec<agent_core::agent::RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.batch_lengths.lock().unwrap().push(events.len());
        Ok(ContextSummary {
            objective: self
                .summary_text
                .clone()
                .unwrap_or_else(|| "Keep investigating".to_owned()),
            ..ContextSummary::default()
        })
    }
}

#[tokio::test]
async fn the_manager_batches_for_the_summary_model_instead_of_the_larger_main_model() {
    use agent_core::agent::RunEvent;
    use agent_runtime::{
        CompactingContextManager, ContextInput, ContextManager, InMemoryContextCheckpointStore,
    };

    let mut run = AgentRun::new("smaller-summary-model", "agent", "Inspect files");
    for step in 0..3 {
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "Inspection rationale. ".repeat(30),
        })
        .unwrap();
        let call_id = format!("read-{step}");
        run.record(RunEvent::ToolCallRequested {
            call_id: call_id.clone(),
            tool_name: "read".to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id,
            output: "Found the relevant section.".to_owned(),
        })
        .unwrap();
    }
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(900)
            .recent_tail_tokens(100)
            .build()
            .unwrap(),
        SmallWindowCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    manager.prepare(input.as_ref()).await.unwrap();
    let batches = manager.compactor().batch_lengths.lock().unwrap();
    assert!(batches.len() > 1);
    assert!(batches.iter().all(|&length| length <= 3));
}

#[tokio::test]
async fn protected_inputs_above_the_soft_trigger_do_not_cause_futile_compaction() {
    use agent_runtime::{
        CompactingContextError, CompactingContextManager, ContextInput, ContextManager,
        InMemoryContextCheckpointStore,
    };

    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(1_000)
            .compaction_threshold(500)
            .compaction_target(300)
            .recent_tail_tokens(100)
            .build()
            .unwrap(),
        SmallWindowCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let run = AgentRun::new("protected-floor", "agent", "x".repeat(700));
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    let prepared = manager.prepare(input.as_ref()).await.unwrap();
    assert_eq!(
        prepared.request.messages,
        request(&"x".repeat(700)).messages
    );
    assert!(manager.compactor().batch_lengths.lock().unwrap().is_empty());

    let run = AgentRun::new("protected-overflow", "agent", "x".repeat(1_001));
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    assert!(matches!(
        manager.prepare(input.as_ref()).await,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    assert!(manager.compactor().batch_lengths.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_accepted_checkpoint_above_target_waits_for_growth_before_compacting_again() {
    use agent_core::agent::RunEvent;
    use agent_runtime::{
        CompactingContextManager, ContextCheckpointStore, ContextInput, ContextManager,
        InMemoryContextCheckpointStore,
    };

    fn complete_step(run: &mut AgentRun, content: String) {
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            content,
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        })
        .unwrap();
        let call_id = format!("c{}", run.current_step());
        run.record(RunEvent::ToolCallRequested {
            call_id: call_id.clone(),
            tool_name: "t".to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id,
            output: "ok".to_owned(),
        })
        .unwrap();
    }

    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(4_000)
            .compaction_threshold(500)
            .compaction_target(300)
            .recent_tail_tokens(100)
            .build()
            .unwrap(),
        SmallWindowCompactor {
            summary_text: Some("s".repeat(450)),
            ..SmallWindowCompactor::default()
        },
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let mut run = AgentRun::new("checkpoint-growth", "agent", "Inspect");
    for _ in 0..3 {
        complete_step(&mut run, "old details ".repeat(50));
    }
    complete_step(&mut run, "Recent".to_owned());
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    manager.prepare(input.as_ref()).await.unwrap();
    let checkpoint = manager
        .checkpoint_store()
        .latest(run.id().to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoint.after_tokens > manager.policy().compaction_threshold());
    let calls = manager.compactor().batch_lengths.lock().unwrap().len();
    assert!(calls > 0);

    complete_step(&mut run, "More".to_owned());
    let input = ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new());
    manager.prepare(input.as_ref()).await.unwrap();
    assert_eq!(
        manager.compactor().batch_lengths.lock().unwrap().len(),
        calls
    );
}
