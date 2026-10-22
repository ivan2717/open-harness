use agent_providers::{HttpModel, ProviderError};
use agent_runtime::{
    ApproximateTokenCounter, ContentBlock, ContextCheckpoint, ContextCheckpointStore,
    ContextInputRef, ContextManager, ContextMeasurement, InMemoryContextCheckpointStore,
    InMemoryContextCheckpointStoreError, Model, ModelCapabilities, ModelContextMetadata,
    ModelOutput, ModelRequest, ModelResponse, PreparedContext, ProviderContextIdentity,
    ProviderResponseState, ProviderStateError, RequestUsageBaseline, TokenCounter,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::Hasher,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Small diagnostics only: never writes request text, reasoning or credentials.
#[derive(Clone)]
pub struct Report {
    path: PathBuf,
    data: Arc<Mutex<Value>>,
}

impl Report {
    pub fn new(path: PathBuf, metadata: Value) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Reserve the destination atomically: a rerun must never replace the
        // evidence from an earlier attempt, including its failure diagnostics.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let report = Self {
            path,
            data: Arc::new(Mutex::new(json!({
                "metadata": metadata,
                "calls": [],
                "measurements": [],
                "checkpoints": [],
                "scenarios": [],
                "complete": false,
            }))),
        };
        report.update(|_| ())?;
        Ok(report)
    }

    pub fn update(&self, apply: impl FnOnce(&mut Value)) -> io::Result<()> {
        let mut data = self.data.lock().unwrap();
        apply(&mut data);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&*data)?)?;
        fs::rename(temporary, &self.path)
    }

    pub fn snapshot(&self) -> Value {
        self.data.lock().unwrap().clone()
    }

    pub fn push(&self, field: &str, value: Value) {
        self.update(|report| report[field].as_array_mut().unwrap().push(value))
            .expect("boundary report must remain writable");
    }
}

pub fn fingerprint(value: &(impl Serialize + ?Sized)) -> String {
    struct Writer(DefaultHasher);
    impl io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(DefaultHasher::new());
    serde_json::to_writer(&mut writer, value).unwrap();
    format!("{:016x}", writer.0.finish())
}

#[derive(Clone)]
pub struct ObservedModel {
    inner: HttpModel,
    report: Report,
    stage: Arc<Mutex<String>>,
}

impl ObservedModel {
    pub fn new(inner: HttpModel, report: Report) -> Self {
        Self {
            inner,
            report,
            stage: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn stage(&self, stage: &str) {
        *self.stage.lock().unwrap() = stage.into();
    }

    pub fn call_count(&self) -> usize {
        self.report.snapshot()["calls"].as_array().unwrap().len()
    }
}

fn summary_mode(request: &ModelRequest) -> Option<&'static str> {
    const PREFIX: &str = "Summarize the supplied run events";
    if request.instructions.starts_with(PREFIX) {
        Some("event_batch")
    } else if request
        .messages
        .last()
        .is_some_and(|message| message.text().starts_with(PREFIX))
    {
        Some("projected_prefix")
    } else {
        None
    }
}

impl Model for ObservedModel {
    type Error = ProviderError;

    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        self.inner.is_context_overflow(error)
    }
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        self.inner.context_metadata()
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        self.inner.provider_context_identity()
    }
    fn requires_provider_state(&self) -> bool {
        self.inner.requires_provider_state()
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        self.inner.validate_replay_state(state, content)
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(self.respond_with_metadata(request).await?.response)
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        let request_fingerprint = fingerprint(&request);
        let raw = ApproximateTokenCounter::default().count_tokens(&request);
        let stage = self.stage.lock().unwrap().clone();
        let mut index = 0;
        let mut admitted = false;
        self.report
            .update(|report| {
                let max_calls =
                    report["metadata"]["max_model_calls"].as_u64().unwrap_or(10) as usize;
                let calls = report["calls"].as_array_mut().unwrap();
                if calls.len() < max_calls {
                    index = calls.len();
                    calls.push(json!({
                        "call": index + 1,
                        "stage": stage,
                        "summary_mode": summary_mode(&request),
                        "request_fingerprint": request_fingerprint,
                        "raw_estimated_input_tokens": raw,
                        "message_count": request.messages.len(),
                        "status": "started",
                    }));
                    admitted = true;
                }
            })
            .expect("write report before paid request");
        if !admitted {
            return Err(ProviderError::InvalidConfig(
                "boundary evaluator request limit reached",
            ));
        }
        eprintln!(
            "boundary runtime call {}: {stage}, raw estimate {raw}",
            index + 1
        );
        let started = Instant::now();
        let result = self.inner.respond_with_metadata(request).await;
        self.report
            .update(|report| {
                let call = &mut report["calls"][index];
                call["elapsed_ms"] = json!(started.elapsed().as_millis());
                call["status"] = json!(if result.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                });
                if let Ok(output) = &result {
                    call["provider_usage"] = json!(output.usage);
                    call["provider_state_retained"] = json!(output.provider_state.is_some());
                    call["finish_reason"] = json!(output.response.finish_reason);
                    if let Some(usage) = output.usage.filter(|usage| usage.input_tokens > 0) {
                        call["raw_signed_error_percent"] =
                            json!((raw as f64 / usage.input_tokens as f64 - 1.0) * 100.0);
                    }
                } else if let Err(error) = &result {
                    call["error"] = json!(error.to_string());
                    call["context_overflow"] = json!(self.is_context_overflow(error));
                }
            })
            .expect("write paid request result");
        result
    }
}

pub struct ObservedContext<C> {
    pub inner: C,
    pub report: Report,
}

impl<C: ContextManager> ContextManager for ObservedContext<C> {
    type Error = C::Error;
    fn max_tool_output_chars(&self) -> Option<usize> {
        self.inner.max_tool_output_chars()
    }
    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
        self.inner.estimate_request_tokens(request)
    }
    fn request_usage_baseline(
        &self,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Option<RequestUsageBaseline> {
        self.inner.request_usage_baseline(metadata, request)
    }
    fn measure_request(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Option<ContextMeasurement> {
        let measurement = self.inner.measure_request(input, request);
        self.report.push("measurements", json!({"run_id": input.run_id, "request_fingerprint": fingerprint(request), "measurement": measurement}));
        measurement
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
    async fn recover_overflow(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        self.report.push("scenarios", json!({"stage": "overflow_recovery_requested", "run_id": input.run_id, "request_fingerprint": fingerprint(request)}));
        self.inner.recover_overflow(input, request).await
    }
}

#[derive(Clone)]
pub struct ObservedCheckpoints {
    pub inner: InMemoryContextCheckpointStore,
    pub report: Report,
}
impl ContextCheckpointStore for ObservedCheckpoints {
    type Error = InMemoryContextCheckpointStoreError;
    async fn save(&self, checkpoint: ContextCheckpoint) -> Result<(), Self::Error> {
        self.inner.save(checkpoint.clone()).await?;
        self.report.push("checkpoints", json!({
            "run_id": checkpoint.run_id,
            "id": checkpoint.id,
            "through_sequence": checkpoint.through_sequence,
            "before_tokens": checkpoint.before_tokens,
            "after_tokens": checkpoint.after_tokens,
            "has_summary": !checkpoint.summary.is_empty(),
            "summary": checkpoint.summary,
            "pruned_results": checkpoint.pruning.as_ref().map(|pruning| pruning.replacements.len()).unwrap_or(0),
        }));
        Ok(())
    }
    async fn latest(&self, run_id: String) -> Result<Option<ContextCheckpoint>, Self::Error> {
        self.inner.latest(run_id).await
    }
    async fn clear(&self, run_id: String) -> Result<(), Self::Error> {
        self.inner.clear(run_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn new_report_refuses_to_overwrite_previous_evidence() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "boundary-report-no-overwrite-{}-{nonce}.json",
            std::process::id()
        ));
        fs::write(&path, "previous evidence").unwrap();
        let error = Report::new(path.clone(), json!({})).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "previous evidence");
        fs::remove_file(path).unwrap();
    }
}
