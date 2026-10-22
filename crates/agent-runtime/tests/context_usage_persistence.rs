mod support;

use std::{
    convert::Infallible,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, ApproximateTokenCounter, CompactingContextManager, ContentBlock, ContextInputRef,
    ContextManager, ContextMeasurement, ContextMeasurementSource, InMemoryContextCheckpointStore,
    JsonlEventStore, Model, ModelContextMetadata, ModelFinishReason, ModelOutput, ModelRequest,
    ModelResponse, ModelUsage, ModelUsageRecord, PreparedContext, ProviderContextIdentity,
    TokenCounter, ToolCall, ToolDefinition, ToolExecutor,
};
use support::{UnexpectedCompactor, test_context_manager};

const REPORTED_USAGE: ModelUsage = ModelUsage {
    input_tokens: 5_000,
    output_tokens: 8_000,
    cached_input_tokens: Some(4_000),
};

type InnerContext = CompactingContextManager<
    UnexpectedCompactor,
    InMemoryContextCheckpointStore,
    ApproximateTokenCounter,
>;

struct RecordingContext {
    inner: InnerContext,
    measurements: Mutex<Vec<ContextMeasurement>>,
}
impl RecordingContext {
    fn new() -> Self {
        Self {
            inner: test_context_manager(),
            measurements: Mutex::new(Vec::new()),
        }
    }
}
impl ContextManager for RecordingContext {
    type Error = <InnerContext as ContextManager>::Error;
    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
        self.inner.estimate_request_tokens(request)
    }
    fn request_usage_baseline(
        &self,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Option<agent_runtime::RequestUsageBaseline> {
        self.inner.request_usage_baseline(metadata, request)
    }
    fn measure_request(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Option<ContextMeasurement> {
        let measurement = self.inner.measure_request(input, request)?;
        self.measurements.lock().unwrap().push(measurement.clone());
        Some(measurement)
    }
    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        self.inner.prepare(input).await
    }
    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        self.inner.observe_response(input, output).await
    }
}

struct OneResponseModel {
    metadata: ModelContextMetadata,
    identity: ProviderContextIdentity,
    output: Mutex<Option<ModelOutput>>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl OneResponseModel {
    fn new(format: &str, calibration: bool, output: ModelOutput) -> Self {
        Self {
            metadata: ModelContextMetadata::new("test-provider", "test-model", 128_000)
                .unwrap()
                .with_input_usage_calibration(calibration),
            identity: ProviderContextIdentity {
                provider: "test-provider".into(),
                model: "test-model".into(),
                format: format.into(),
            },
            output: Mutex::new(Some(output)),
            requests: Mutex::new(Vec::new()),
        }
    }
}
impl Model for OneResponseModel {
    type Error = Infallible;
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("usage must be preserved")
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self.output.lock().unwrap().take().unwrap())
    }
}

struct NoTools;
impl ToolExecutor for NoTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        panic!("no tools requested")
    }
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "open-harness-usage-{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct RoundTrip {
    original_request: ModelRequest,
    continued_request: ModelRequest,
    measurement: ContextMeasurement,
    usage_record: ModelUsageRecord,
}

async fn restart_and_continue(next_protocol: &str, calibration: bool) -> RoundTrip {
    let directory = TestDirectory::new();
    let agent = Agent::new(
        "persist-agent",
        "Persistence",
        "",
        "Follow the user instructions.",
    );
    let (original_request, expected_record) = {
        let mut output = ModelOutput::from(ModelResponse {
            content: vec![
                ContentBlock::Reasoning {
                    text: "Retained reasoning summary.".into(),
                },
                ContentBlock::Text {
                    text: "Which file should I inspect?".into(),
                },
            ],
            finish_reason: ModelFinishReason::InputRequired,
        });
        // Generated usage can include provider-side reasoning that is not
        // replayed verbatim. Only retained canonical blocks enter the next input.
        output.usage = Some(REPORTED_USAGE);
        let runtime = AgentRuntime::with_event_store(
            OneResponseModel::new("chat-v1", calibration, output),
            NoTools,
            RecordingContext::new(),
            JsonlEventStore::new(&directory.0),
            Some(4),
        );
        let mut run = runtime
            .start_run(&agent, "durable-usage", "Inspect a file.")
            .await
            .unwrap();
        assert_eq!(
            runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
            RunStatus::WaitingForInput
        );
        let usage_record = run
            .events()
            .iter()
            .find_map(|event| match &event.event {
                RunEvent::ModelResponseReceived { token_usage, .. } => {
                    token_usage.as_deref().cloned()
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(usage_record.usage, REPORTED_USAGE);
        assert_eq!(usage_record.baseline.is_some(), calibration);
        let original_request = runtime.model().requests.lock().unwrap()[0].clone();
        (original_request, usage_record)
    }; // Drop the first runtime, context usage cache, and JSONL store completely.

    let runtime = AgentRuntime::with_event_store(
        OneResponseModel::new(
            next_protocol,
            calibration,
            ModelResponse::final_answer("Inspected.".into()).into(),
        ),
        NoTools,
        RecordingContext::new(),
        JsonlEventStore::new(&directory.0),
        Some(4),
    );
    let mut restored = runtime.restore_run("durable-usage").await.unwrap().unwrap();
    assert_eq!(restored.status(), RunStatus::WaitingForInput);
    let durable_record = restored
        .events()
        .iter()
        .find_map(|event| match &event.event {
            RunEvent::ModelResponseReceived { token_usage, .. } => token_usage.as_deref().cloned(),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        durable_record, expected_record,
        "usage and its exact request binding must survive reopening JSONL"
    );
    assert!(
        runtime
            .context_manager()
            .measurements
            .lock()
            .unwrap()
            .is_empty()
    );
    runtime
        .submit_input(&mut restored, "Inspect Cargo.toml.")
        .await
        .unwrap();
    assert_eq!(
        runtime
            .run_until_blocked(&agent, &mut restored)
            .await
            .unwrap(),
        RunStatus::Completed
    );
    let measurements = runtime.context_manager().measurements.lock().unwrap();
    assert_eq!(measurements.len(), 1);
    let continued_request = runtime.model().requests.lock().unwrap()[0].clone();
    RoundTrip {
        original_request,
        continued_request,
        measurement: measurements[0].clone(),
        usage_record: durable_record,
    }
}

#[tokio::test]
async fn jsonl_restart_restores_provider_calibration_without_double_counting_cache_or_reasoning() {
    let round_trip = restart_and_continue("chat-v1", true).await;
    let counter = ApproximateTokenCounter::default();
    let raw = counter.count_tokens(&round_trip.continued_request);
    let prefix = counter.count_tokens(&round_trip.original_request);
    let durable_baseline = round_trip.usage_record.baseline.as_ref().unwrap();
    assert_eq!(durable_baseline.raw_estimated_input_tokens, Some(prefix));
    assert_eq!(
        durable_baseline.estimator_identity,
        counter.estimator_identity()
    );
    let retained_delta = raw - prefix;
    let measurement = &round_trip.measurement;
    assert_eq!(
        measurement.source,
        ContextMeasurementSource::ProviderCalibrated
    );
    assert_eq!(
        measurement.measured_input_tokens,
        Some(REPORTED_USAGE.input_tokens)
    );
    assert_eq!(measurement.raw_estimated_input_tokens, raw);
    assert_eq!(measurement.estimated_delta_tokens, retained_delta);
    assert_eq!(
        measurement.estimated_input_tokens,
        REPORTED_USAGE.input_tokens + retained_delta
    );
    assert!(
        retained_delta < REPORTED_USAGE.output_tokens,
        "only retained response content and new input are estimated, not all generated reasoning/output"
    );
    assert_ne!(
        measurement.estimated_input_tokens, raw,
        "the new manager must use the durable measured prefix instead of reverting to a raw estimate"
    );
    assert_eq!(
        measurement.estimation_margin_tokens,
        (retained_delta * 15).div_ceil(100)
    );
    assert_eq!(
        measurement.decision_input_tokens,
        REPORTED_USAGE.input_tokens + retained_delta + measurement.estimation_margin_tokens
    );
    assert_eq!(
        round_trip.usage_record.usage.cached_input_tokens,
        Some(4_000),
        "cached input remains a subset of the measured 5,000 input tokens"
    );
}

#[tokio::test]
async fn reopening_the_same_model_through_another_protocol_invalidates_the_durable_baseline() {
    let round_trip = restart_and_continue("responses-v1", true).await;
    assert!(
        round_trip.usage_record.baseline.is_some(),
        "the first protocol produced a valid baseline"
    );
    assert_eq!(
        round_trip.measurement.source,
        ContextMeasurementSource::Estimated
    );
    assert_eq!(round_trip.measurement.measured_input_tokens, None);
    assert_eq!(
        round_trip.measurement.estimated_input_tokens,
        ApproximateTokenCounter::default().count_tokens(&round_trip.continued_request)
    );
}

#[tokio::test]
async fn spending_only_usage_survives_restart_without_becoming_a_context_measurement() {
    let round_trip = restart_and_continue("chat-v1", false).await;
    assert_eq!(round_trip.usage_record.usage, REPORTED_USAGE);
    assert_eq!(round_trip.usage_record.baseline, None);
    assert_eq!(
        round_trip.measurement.source,
        ContextMeasurementSource::Estimated
    );
    assert_eq!(round_trip.measurement.measured_input_tokens, None);
    assert_eq!(
        round_trip.measurement.estimated_input_tokens,
        round_trip.measurement.raw_estimated_input_tokens
    );
}
