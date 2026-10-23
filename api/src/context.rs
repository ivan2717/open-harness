use crate::{
    contracts::{CompactionRecord, ContextSnapshot},
    demo::WINDOW_TOKENS,
    snapshot::{Snapshot, write_json},
    time,
};
use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCheckpoint, ContextCheckpointStore,
    ContextCompactor, ContextInputRef, ContextManager, ContextMeasurement,
    ContextMeasurementSource, ContextPolicy, ContextSummary, ModelContextMetadata, ModelOutput,
    ModelRequest, PreparedContext, RequestUsageBaseline, TokenCounter,
};
use std::{
    convert::Infallible,
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[path = "context_memory.rs"]
mod memory_hook;
use memory_hook::MemoryCompactionHook;

#[cfg(test)]
#[path = "context_memory_tests.rs"]
mod memory_tests;

pub fn policy() -> ContextPolicy {
    ContextPolicy::builder()
        .model_context_window(WINDOW_TOKENS)
        .output_token_reserve(1024)
        .safety_margin(256)
        .compaction_threshold(3000)
        .compaction_target(1800)
        .recent_tail_tokens(900)
        .max_tool_output_chars(10_000)
        .build()
        .expect("valid local demonstration policy")
}

pub fn initial_context() -> ContextSnapshot {
    let policy = policy();
    ContextSnapshot {
        window_tokens: WINDOW_TOKENS,
        input_budget: policy.input_token_limit(),
        estimated_input_tokens: 0,
        trigger_tokens: policy.compaction_threshold(),
        target_tokens: policy.compaction_target(),
        reserved_output_tokens: policy.model_budget().output_token_reserve(),
        compaction_count: 0,
        measurement: "estimated".into(),
        accounting: None,
        observed_usage: None,
        checkpoints: Vec::new(),
    }
}

pub struct DemoCompactor;
impl ContextCompactor for DemoCompactor {
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
        previous: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        let mut summary = previous.unwrap_or_default();
        summary.objective =
            "Observe the local demonstration runtime and context compaction.".into();
        for event in events {
            if let RunEvent::ToolCallSucceeded { call_id, .. } = event.event {
                summary
                    .completed_work
                    .push(format!("Diagnostic sample {call_id} completed."));
            }
        }
        summary.current_state = "Older diagnostic payloads were summarized by a deterministic local extractor; original events remain in JSONL.".into();
        summary.next_action =
            "Continue the remaining diagnostic rounds, then report the demo complete.".into();
        Ok(summary)
    }
}

pub struct DiskCheckpoints {
    path: PathBuf,
    checkpoints: Arc<Mutex<Vec<ContextCheckpoint>>>,
    snapshot: Snapshot,
}
impl DiskCheckpoints {
    pub fn new(path: PathBuf, snapshot: Snapshot) -> io::Result<Self> {
        let checkpoints: Vec<ContextCheckpoint> = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?
        } else {
            Vec::new()
        };
        // A checkpoint write can commit before its derived UI snapshot does.
        // Rebuild this view from the authoritative checkpoint file on startup.
        let saved_at = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .map(time::timestamp)
            .unwrap_or_else(|_| time::now());
        reconcile_checkpoints(&snapshot, &checkpoints, &saved_at)?;
        Ok(Self {
            path,
            checkpoints: Arc::new(Mutex::new(checkpoints)),
            snapshot,
        })
    }
}

fn reconcile_checkpoints(
    snapshot: &Snapshot,
    checkpoints: &[ContextCheckpoint],
    fallback_time: &str,
) -> io::Result<()> {
    snapshot.update(|detail| {
        let context = detail.context.get_or_insert_with(initial_context);
        let mut restored = Vec::new();
        for checkpoint in checkpoints.iter().filter(|checkpoint| {
            checkpoint.run_id == detail.summary.id && !checkpoint.summary.is_empty()
        }) {
            // Pruning can retain an existing summary while saving a newer tool
            // projection. Count each summary boundary once in the inspector.
            if restored.iter().any(|existing: &&ContextCheckpoint| {
                existing.through_sequence == checkpoint.through_sequence
                    && existing.summary == checkpoint.summary
            }) {
                continue;
            }
            restored.push(checkpoint);
        }
        context.checkpoints = restored
            .into_iter()
            .map(|checkpoint| {
                let created_at = context
                    .checkpoints
                    .iter()
                    .find(|existing| existing.id == checkpoint.id)
                    .map_or_else(
                        || fallback_time.to_owned(),
                        |existing| existing.created_at.clone(),
                    );
                CompactionRecord {
                    id: checkpoint.id.clone(),
                    before_tokens: checkpoint.before_tokens,
                    after_tokens: checkpoint.after_tokens,
                    summary: checkpoint.summary.render(),
                    created_at,
                }
            })
            .collect();
        context.compaction_count = context.checkpoints.len();
    })
}
impl ContextCheckpointStore for DiskCheckpoints {
    type Error = io::Error;
    async fn save(&self, checkpoint: ContextCheckpoint) -> Result<(), Self::Error> {
        {
            let mut checkpoints = self
                .checkpoints
                .lock()
                .map_err(|_| io::Error::other("checkpoint lock poisoned"))?;
            if let Some(existing) = checkpoints
                .iter()
                .find(|existing| existing.id == checkpoint.id)
            {
                if existing != &checkpoint {
                    return Err(io::Error::other("conflicting context checkpoint id"));
                }
            } else {
                let mut candidate = checkpoints.clone();
                candidate.push(checkpoint);
                write_json(&self.path, &candidate)?;
                *checkpoints = candidate;
            }
            reconcile_checkpoints(&self.snapshot, &checkpoints, &time::now())?;
        }
        Ok(())
    }
    async fn latest(&self, run_id: String) -> Result<Option<ContextCheckpoint>, Self::Error> {
        Ok(self
            .checkpoints
            .lock()
            .map_err(|_| io::Error::other("checkpoint lock poisoned"))?
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.run_id == run_id)
            .cloned())
    }
    async fn clear(&self, run_id: String) -> Result<(), Self::Error> {
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| io::Error::other("checkpoint lock poisoned"))?;
        let candidate = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.run_id != run_id)
            .cloned()
            .collect::<Vec<_>>();
        write_json(&self.path, &candidate)?;
        *checkpoints = candidate;
        reconcile_checkpoints(&self.snapshot, &checkpoints, &time::now())
    }
}

pub struct ObservedContext<C = DemoCompactor> {
    inner: CompactingContextManager<C, DiskCheckpoints, ApproximateTokenCounter>,
    snapshot: Snapshot,
    memory: Option<Arc<crate::memory::WikiMemory>>,
}
impl ObservedContext {
    #[cfg(test)]
    pub fn new(path: PathBuf, snapshot: Snapshot) -> io::Result<Self> {
        Self::with_compactor(path, snapshot, policy(), DemoCompactor)
    }
}
impl<C: ContextCompactor> ObservedContext<C> {
    pub fn with_compactor(
        path: PathBuf,
        snapshot: Snapshot,
        policy: ContextPolicy,
        compactor: C,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: CompactingContextManager::new(
                policy,
                compactor,
                DiskCheckpoints::new(path, snapshot.clone())?,
                ApproximateTokenCounter::default(),
            ),
            snapshot,
            memory: None,
        })
    }

    pub fn with_memory(mut self, memory: Arc<crate::memory::WikiMemory>) -> Self {
        self.memory = Some(memory);
        self
    }

    fn derived_messages(&self, input: ContextInputRef<'_>) -> Vec<agent_runtime::ModelMessage> {
        let mut messages = input.derived_messages.to_vec();
        if let Some(memory) = &self.memory {
            messages.extend(memory.messages(input));
        }
        messages
    }

    pub fn abort_memory_background(&self) {
        if let Some(memory) = &self.memory {
            memory.abort_background();
        }
    }

    pub async fn stop_memory_background(&self) {
        if let Some(memory) = &self.memory {
            memory.stop_background().await;
        }
    }

    fn record_measurement(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> io::Result<()> {
        let Some(measurement) = self.inner.measure_request(input, request) else {
            return Ok(());
        };
        let policy = self
            .inner
            .policy()
            .resolve(input.model_metadata)
            .map_err(io::Error::other)?;
        self.snapshot.update(|detail| {
            let context = detail.context.get_or_insert_with(initial_context);
            context.window_tokens = policy.model_budget().context_window();
            context.input_budget = policy.input_token_limit();
            context.trigger_tokens = policy.compaction_threshold();
            context.target_tokens = policy.compaction_target();
            context.reserved_output_tokens = policy.model_budget().output_token_reserve();
            context.estimated_input_tokens = measurement.estimated_input_tokens;
            context.measurement = match measurement.source {
                ContextMeasurementSource::Estimated => "estimated",
                ContextMeasurementSource::ProviderCalibrated => "provider_calibrated",
                ContextMeasurementSource::ProviderNative => "provider_native",
                // Compatibility with measurements saved by the retired full-count path.
                ContextMeasurementSource::ProviderCounted => "provider_counted",
                ContextMeasurementSource::TokenizerEstimated => "tokenizer_estimated",
            }
            .into();
            context.accounting = Some(measurement);
        })
    }
}

// Wiki notes are optional input. A failed attempt to fit/summarize them may
// retry the original projection once, while protocol and journal integrity
// failures must remain visible to the caller.
fn can_omit_wiki<C, S>(error: &agent_runtime::CompactingContextError<C, S>) -> bool {
    use agent_runtime::CompactingContextError;
    matches!(
        error,
        CompactingContextError::Compactor(_)
            | CompactingContextError::EmptySummary { .. }
            | CompactingContextError::NoEventsToCompact
            | CompactingContextError::CompactionInputTokenLimitExceeded { .. }
            | CompactingContextError::CompactionDidNotReduceTokens { .. }
            | CompactingContextError::InputTokenLimitExceeded { .. }
            | CompactingContextError::ProtectedContextTokenLimitExceeded { .. }
    )
}

impl<C: ContextCompactor> ContextManager for ObservedContext<C> {
    type Error = io::Error;
    fn max_tool_output_chars(&self) -> Option<usize> {
        self.inner.max_tool_output_chars()
    }
    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
        self.inner.estimate_request_tokens(request)
    }
    fn measure_request(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Option<ContextMeasurement> {
        self.inner.measure_request(input, request)
    }
    fn request_usage_baseline(
        &self,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Option<RequestUsageBaseline> {
        self.inner.request_usage_baseline(metadata, request)
    }
    async fn compact_now(
        &self,
        input: ContextInputRef<'_>,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        let derived = self.derived_messages(input);
        let hook = MemoryCompactionHook::new(self.memory.as_ref(), input, &derived);
        let enriched = input.with_derived_messages(&derived);
        let prepared = self.inner.compact_now_with_hook(enriched, &hook).await;
        let prepared = match prepared {
            Err(error) if hook.injected_memory() && can_omit_wiki(&error) => {
                hook.omit_memory();
                self.inner.compact_now_with_hook(input, &hook).await
            }
            result => result,
        }
        .map_err(io::Error::other)?;
        if let Some(prepared) = &prepared {
            self.record_measurement(input, &prepared.request)?;
        }
        Ok(prepared)
    }
    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        self.inner
            .observe_response(input, output)
            .await
            .map_err(io::Error::other)?;
        if output.response.calls().is_empty()
            && let Some(memory) = &self.memory
        {
            memory.schedule(input, crate::memory::UpdateReason::Response);
        }
        Ok(())
    }
    async fn recover_overflow(
        &self,
        input: ContextInputRef<'_>,
        rejected: &ModelRequest,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        let derived = self.derived_messages(input);
        let hook = MemoryCompactionHook::new(self.memory.as_ref(), input, &derived);
        let prepared = self
            .inner
            .recover_overflow_with_hook(input.with_derived_messages(&derived), rejected, &hook)
            .await;
        let prepared = match prepared {
            Err(error) if hook.injected_memory() && can_omit_wiki(&error) => {
                hook.omit_memory();
                self.inner
                    .recover_overflow_with_hook(input, rejected, &hook)
                    .await
            }
            result => result,
        }
        .map_err(io::Error::other)?;
        if let Some(prepared) = &prepared {
            self.record_measurement(input, &prepared.request)?;
        }
        Ok(prepared)
    }
    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        if let Some(memory) = &self.memory {
            memory.schedule(input, crate::memory::UpdateReason::Progress);
        }
        let derived = self.derived_messages(input);
        let hook = MemoryCompactionHook::new(self.memory.as_ref(), input, &derived);
        let prepared = self
            .inner
            .prepare_with_hook(input.with_derived_messages(&derived), &hook)
            .await;
        let prepared = match prepared {
            Err(error) if hook.injected_memory() && can_omit_wiki(&error) => {
                hook.omit_memory();
                self.inner.prepare_with_hook(input, &hook).await
            }
            result => result,
        }
        .map_err(io::Error::other)?;
        self.record_measurement(input, &prepared.request)?;
        Ok(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{RunDetail, RunStatus, RunSummary};
    use agent_runtime::{ContextInput, ModelContextMetadata, ModelResponse, ModelUsage};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    static NEXT_FALLBACK_TEST: AtomicU64 = AtomicU64::new(0);

    struct FallbackDirectory(PathBuf);
    impl FallbackDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "open-harness-wiki-fallback-{}-{}",
                std::process::id(),
                NEXT_FALLBACK_TEST.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for FallbackDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct UnavailableCompactor(Arc<AtomicUsize>);
    impl ContextCompactor for UnavailableCompactor {
        type Error = io::Error;
        fn estimate_input_tokens(
            &self,
            _: Option<&ContextSummary>,
            _: &[RunEventEnvelope],
            _: &dyn TokenCounter,
        ) -> io::Result<usize> {
            Ok(0)
        }
        async fn compact(
            &self,
            _: Option<ContextSummary>,
            _: Vec<RunEventEnvelope>,
        ) -> io::Result<ContextSummary> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::other("summary service unavailable"))
        }
    }

    fn fallback_context(
        directory: &FallbackDirectory,
        assistant_chars: usize,
    ) -> (
        ObservedContext<UnavailableCompactor>,
        ContextInput,
        Arc<AtomicUsize>,
    ) {
        let mut run = agent_core::agent::AgentRun::new("fallback", "agent", "Continue the task");
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            provider_state: None,
            token_usage: None,
            content: "x".repeat(assistant_chars),
        })
        .unwrap();
        let input = ContextInput::new("fallback", "", run.events().to_vec(), vec![])
            .with_derived_messages(vec![agent_runtime::ModelMessage::summary(
                "caller-context".into(),
                "Required caller context".into(),
            )]);
        let host = crate::extensions::ExtensionHost::open(&directory.0.join("host")).unwrap();
        host.memory
            .apply(
                "fallback",
                &input.events,
                input.events.last().unwrap().sequence,
                agent_memory::WikiUpdate {
                    overview: "Optional historical wiki notes. ".repeat(60),
                    pages: vec![],
                    tasks: vec![],
                },
            )
            .unwrap();
        let memory = Arc::new(crate::memory::WikiMemory::new(
            host,
            crate::model::ServiceModel::Unavailable,
        ));
        let snapshot = Snapshot::new(
            RunDetail {
                summary: RunSummary {
                    compacting: false,
                    id: "fallback".into(),
                    revision: 0,
                    title: "fallback".into(),
                    prompt: "Continue the task".into(),
                    status: RunStatus::Running,
                    model_id: "test".into(),
                    workspace_path: None,
                    permission_mode: Default::default(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                messages: vec![],
                events: vec![],
                context: None,
                error: None,
            },
            directory.0.join("snapshot.json"),
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let manager = ObservedContext::with_compactor(
            directory.0.join("checkpoints.json"),
            snapshot,
            ContextPolicy::builder()
                .input_token_limit(1_200)
                .compaction_threshold(1_100)
                .recent_tail_tokens(1)
                .estimation_margin_percent(0)
                .build()
                .unwrap(),
            UnavailableCompactor(calls.clone()),
        )
        .unwrap()
        .with_memory(memory);
        (manager, input, calls)
    }

    #[tokio::test]
    async fn optional_wiki_can_be_omitted_when_it_alone_requires_a_failing_compaction() {
        let directory = FallbackDirectory::new();
        // Even the lean archive locator can push an almost-full request past
        // the hard limit. Omitting it must preserve the original caller input.
        let (manager, input, calls) = fallback_context(&directory, 4_000);
        let original = manager.inner.prepare(input.as_ref()).await.unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let prepared = manager.prepare(input.as_ref()).await.unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(prepared.request, original.request);
        assert_eq!(prepared.request.messages[0], input.derived_messages[0]);
        assert!(
            prepared
                .request
                .messages
                .iter()
                .all(|message| !message.text().contains("Optional historical wiki notes"))
        );
        assert!(
            manager
                .inner
                .checkpoint_store()
                .latest(input.run_id.clone())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn wiki_fallback_keeps_original_failures_and_does_not_retry_without_extra_memory() {
        for has_wiki in [true, false] {
            let directory = FallbackDirectory::new();
            let (mut manager, input, calls) = fallback_context(&directory, 10_000);
            if !has_wiki {
                manager.memory = None;
            }
            let source = input.events.clone();

            let error = manager.prepare(input.as_ref()).await.unwrap_err();

            assert!(error.to_string().contains("summary service unavailable"));
            assert_eq!(calls.load(Ordering::Relaxed), if has_wiki { 2 } else { 1 });
            assert_eq!(input.events, source);
        }
    }

    #[tokio::test]
    async fn wiki_fallback_preserves_explicit_compaction_and_overflow_recovery_requirements() {
        for manual in [true, false] {
            let directory = FallbackDirectory::new();
            let (manager, input, calls) = fallback_context(&directory, 3_600);
            let original = manager.inner.prepare(input.as_ref()).await.unwrap();
            let error = if manual {
                manager.compact_now(input.as_ref()).await.unwrap_err()
            } else {
                manager
                    .recover_overflow(input.as_ref(), &original.request)
                    .await
                    .unwrap_err()
            };
            assert!(error.to_string().contains("summary service unavailable"));
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        }
    }

    #[tokio::test]
    async fn observed_context_forwards_usage_and_publishes_the_decision_measurement() {
        let directory =
            std::env::temp_dir().join(format!("open-harness-context-meter-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let detail = RunDetail {
            summary: RunSummary {
                compacting: false,
                id: "meter".into(),
                revision: 0,
                title: "meter".into(),
                prompt: "test".into(),
                status: RunStatus::Running,
                model_id: "local-demo".into(),
                workspace_path: None,
                permission_mode: Default::default(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let snapshot = Snapshot::new(detail, directory.join("snapshot.json")).unwrap();
        let manager =
            ObservedContext::new(directory.join("checkpoints.json"), snapshot.clone()).unwrap();
        let input = ContextInput::new(
            "meter",
            "system",
            vec![RunEventEnvelope {
                sequence: 0,
                step: 0,
                occurred_at: std::time::SystemTime::UNIX_EPOCH,
                event: RunEvent::RunStarted {
                    instruction: "test".into(),
                    files: Vec::new(),
                },
            }],
            Vec::new(),
        )
        .with_model_metadata(ModelContextMetadata::new("test", "model", WINDOW_TOKENS).unwrap());
        let prepared = manager.prepare(input.as_ref()).await.unwrap();
        let output = ModelOutput {
            usage: Some(ModelUsage {
                input_tokens: 1000,
                output_tokens: 200,
                cached_input_tokens: Some(500),
            }),
            ..ModelResponse::final_answer("done".into()).into()
        };
        manager
            .observe_response(input.as_ref(), &output)
            .await
            .unwrap();
        let measured = manager
            .measure_request(input.as_ref(), &prepared.request)
            .unwrap();
        assert_eq!(measured.measured_input_tokens, Some(1000));
        assert_eq!(measured.decision_input_tokens, 1000);
        manager.prepare(input.as_ref()).await.unwrap();
        let context = snapshot.get().context.unwrap();
        assert_eq!(context.measurement, "provider_calibrated");
        assert_eq!(context.estimated_input_tokens, 1000);
        assert_eq!(context.accounting, Some(measured));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn disk_pruning_is_restored_in_write_order_without_counting_as_new_summaries() {
        let directory = std::env::temp_dir().join(format!(
            "open-harness-pruning-checkpoints-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let snapshot = Snapshot::new(
            RunDetail {
                summary: RunSummary {
                    compacting: false,
                    id: "disk".into(),
                    revision: 0,
                    title: "disk".into(),
                    prompt: "test".into(),
                    status: RunStatus::WaitingForInput,
                    model_id: "local-demo".into(),
                    workspace_path: None,
                    permission_mode: Default::default(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                messages: Vec::new(),
                events: Vec::new(),
                context: Some(initial_context()),
                error: None,
            },
            directory.join("snapshot.json"),
        )
        .unwrap();
        let path = directory.join("checkpoints.json");
        let store = DiskCheckpoints::new(path.clone(), snapshot.clone()).unwrap();
        let pruning = agent_runtime::ContextPruning {
            through_sequence: 10,
            source_fingerprint: "events".into(),
            replacements: vec![agent_runtime::PrunedToolOutput {
                sequence: 8,
                content: "bounded".into(),
                is_error: false,
            }],
        };
        let pruned = ContextCheckpoint {
            run_id: "disk".into(),
            id: "pruning".into(),
            through_sequence: 10,
            source_fingerprint: "events".into(),
            before_tokens: 1_000,
            after_tokens: 700,
            summary: ContextSummary::default(),
            pruning: Some(pruning.clone()),
        };
        store.save(pruned).await.unwrap();
        assert_eq!(snapshot.get().context.unwrap().compaction_count, 0);
        let summarized = ContextCheckpoint {
            run_id: "disk".into(),
            id: "summary".into(),
            through_sequence: 3,
            source_fingerprint: "summary-events".into(),
            before_tokens: 700,
            after_tokens: 400,
            summary: ContextSummary {
                objective: "Continue".into(),
                ..Default::default()
            },
            pruning: Some(pruning),
        };
        store.save(summarized.clone()).await.unwrap();
        let restored = DiskCheckpoints::new(path.clone(), snapshot.clone()).unwrap();
        assert_eq!(
            restored.latest("disk".into()).await.unwrap(),
            Some(summarized.clone())
        );
        assert_eq!(snapshot.get().context.unwrap().compaction_count, 1);
        let revised = ContextCheckpoint {
            id: "summary-plus-pruning".into(),
            after_tokens: 300,
            ..summarized.clone()
        };
        restored.save(revised.clone()).await.unwrap();
        assert_eq!(snapshot.get().context.unwrap().compaction_count, 1);
        assert_eq!(restored.latest("disk".into()).await.unwrap(), Some(revised));
        assert!(
            restored
                .save(ContextCheckpoint {
                    after_tokens: 999,
                    ..summarized
                })
                .await
                .is_err()
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
