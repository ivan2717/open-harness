use crate::{
    contracts::{CompactionRecord, ContextSnapshot},
    demo::WINDOW_TOKENS,
    snapshot::{Snapshot, write_json},
    time,
};
use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCheckpoint, ContextCheckpointStore,
    ContextCompactor, ContextInputRef, ContextManager, ContextPolicy, ContextSummary, ModelRequest,
    PreparedContext, TokenCounter,
};
use std::{
    convert::Infallible,
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
        let mut restored = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.run_id == detail.summary.id)
            .collect::<Vec<_>>();
        restored.sort_by_key(|checkpoint| checkpoint.through_sequence);
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
            if !checkpoints
                .iter()
                .any(|existing| existing.id == checkpoint.id)
            {
                checkpoints.push(checkpoint.clone());
            }
            write_json(&self.path, &*checkpoints)?;
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
            .filter(|checkpoint| checkpoint.run_id == run_id)
            .max_by_key(|checkpoint| checkpoint.through_sequence)
            .cloned())
    }
    async fn clear(&self, run_id: String) -> Result<(), Self::Error> {
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| io::Error::other("checkpoint lock poisoned"))?;
        checkpoints.retain(|checkpoint| checkpoint.run_id != run_id);
        write_json(&self.path, &*checkpoints)?;
        reconcile_checkpoints(&self.snapshot, &checkpoints, &time::now())
    }
}

pub struct ObservedContext {
    inner: CompactingContextManager<DemoCompactor, DiskCheckpoints, ApproximateTokenCounter>,
    snapshot: Snapshot,
}
impl ObservedContext {
    pub fn new(path: PathBuf, snapshot: Snapshot) -> io::Result<Self> {
        Ok(Self {
            inner: CompactingContextManager::new(
                policy(),
                DemoCompactor,
                DiskCheckpoints::new(path, snapshot.clone())?,
                ApproximateTokenCounter::default(),
            ),
            snapshot,
        })
    }
}
impl ContextManager for ObservedContext {
    type Error = io::Error;
    fn max_tool_output_chars(&self) -> Option<usize> {
        self.inner.max_tool_output_chars()
    }
    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
        self.inner.estimate_request_tokens(request)
    }
    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        let prepared = self.inner.prepare(input).await.map_err(io::Error::other)?;
        let tokens = self.inner.token_counter().count_tokens(&prepared.request);
        self.snapshot.update(|detail| {
            detail
                .context
                .get_or_insert_with(initial_context)
                .estimated_input_tokens = tokens;
        })?;
        Ok(prepared)
    }
}
