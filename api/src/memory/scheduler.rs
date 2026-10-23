//! One coalescing background worker per session and bounded foreground catch-up.
use super::{MemoryEngine, UpdateOutcome, UpdateReason, WikiMemory};
use agent_runtime::{ContextInput, ContextInputRef, Model};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    task::JoinHandle,
    time::{Instant, sleep_until, timeout_at},
};

const DEBOUNCE: Duration = Duration::from_millis(500);
const BACKGROUND_INTERVAL: Duration = Duration::from_secs(30);

/// Shared by all boundaries encountered during one context preparation.
/// Limits cover lock waits and network time, not only successful generations.
pub struct CatchUpBudget {
    deadline: Option<Instant>,
    max_wait: Duration,
    remaining_requests: usize,
}

impl Default for CatchUpBudget {
    fn default() -> Self {
        Self::new(Duration::from_secs(8), 4)
    }
}
impl CatchUpBudget {
    pub fn new(max_wait: Duration, max_requests: usize) -> Self {
        Self {
            deadline: None,
            max_wait,
            remaining_requests: max_requests,
        }
    }
    fn deadline(&mut self) -> Instant {
        *self
            .deadline
            .get_or_insert_with(|| Instant::now() + self.max_wait)
    }
}

#[derive(Default)]
pub(super) struct Background {
    state: Mutex<BackgroundState>,
}
#[derive(Default)]
struct BackgroundState {
    pending: Option<Pending>,
    worker: Option<JoinHandle<()>>,
    epoch: u64,
    last_activity: Option<Instant>,
}
struct Pending {
    input: ContextInput,
    reason: UpdateReason,
    generation: u64,
    failed_retries: usize,
}
impl Background {
    fn take_worker(&self, discard_pending: bool) -> Option<JoinHandle<()>> {
        let mut state = self.state.lock().unwrap();
        if discard_pending {
            state.pending = None;
        }
        state.epoch = state.epoch.wrapping_add(1);
        let worker = state.worker.take();
        if let Some(worker) = &worker {
            worker.abort();
        }
        worker
    }
    pub(super) fn abort(&self) {
        self.take_worker(true);
    }
}

impl<M: Model + 'static> WikiMemory<M> {
    /// Queue only the newest canonical snapshot; ordinary task execution never
    /// waits for memory generation. No tools/instructions are retained here.
    pub fn schedule(&self, input: ContextInputRef<'_>, reason: UpdateReason) {
        let Some(generation) = self.core.host.memory_generation() else {
            return;
        };
        if input.events.is_empty() {
            return;
        }
        let mut state = self.background.state.lock().unwrap();
        let newer_pending = state.pending.as_ref().is_some_and(|pending| {
            pending.generation == generation
                && pending.input.events.last().map(|event| event.sequence)
                    > input.events.last().map(|event| event.sequence)
        });
        let reason = state
            .pending
            .as_ref()
            .filter(|pending| pending.generation == generation)
            .map_or(reason, |pending| merge_reason(pending.reason, reason));
        if newer_pending {
            state.pending.as_mut().unwrap().reason = reason;
        } else {
            state.pending = Some(Pending {
                input: ContextInput::new(input.run_id, "", input.events.to_vec(), vec![]),
                reason,
                generation,
                failed_retries: 0,
            });
        }
        if state
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return;
        }
        state.epoch = state.epoch.wrapping_add(1);
        let epoch = state.epoch;
        let core = self.core.clone();
        let background = self.background.clone();
        state.worker = Some(tokio::spawn(async move {
            worker(core, background, epoch).await
        }));
    }

    /// Synchronous revocation for configuration changes and Drop. Generation
    /// guards also prevent a cancelled request from committing old settings.
    pub fn abort_background(&self) {
        self.background.abort();
    }

    /// Stop and settle a queued/in-flight job before cancellation, shutdown,
    /// model replacement or foreground takeover. Future work can queue again.
    pub async fn stop_background(&self) {
        self.settle_background(true).await;
    }

    async fn settle_background(&self, discard_pending: bool) {
        let worker = self.background.take_worker(discard_pending);
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    /// Bring source coverage up to the exact proposed summary boundary. Reads
    /// validate the whole journal; only compiler source input is prefix-limited.
    pub async fn ensure_coverage(
        &self,
        input: ContextInputRef<'_>,
        through_sequence: u64,
        budget: &mut CatchUpBudget,
    ) -> bool {
        if !self.core.host.memory_enabled() {
            return true;
        }
        if input
            .events
            .last()
            .is_none_or(|event| event.sequence < through_sequence)
        {
            return false;
        }
        match self.core.pending(input, Some(through_sequence)) {
            Ok(false) => return true,
            Err(_) => {
                self.mark_pending(input.run_id);
                return false;
            }
            Ok(true) => (),
        }
        let deadline = budget.deadline();
        // Takeover drops the background model future before acquiring the same
        // compilation lock, so a slow background job cannot consume the budget.
        if timeout_at(deadline, self.settle_background(false))
            .await
            .is_err()
        {
            self.mark_pending(input.run_id);
            self.schedule(input, UpdateReason::Compaction);
            return false;
        }
        let mut ready = false;
        loop {
            if !self.core.host.memory_enabled() {
                ready = true;
                break;
            }
            match self.core.pending(input, Some(through_sequence)) {
                Ok(false) => {
                    ready = true;
                    break;
                }
                Err(_) => break,
                Ok(true) => (),
            }
            if budget.remaining_requests == 0 || Instant::now() >= deadline {
                break;
            }
            budget.remaining_requests -= 1;
            let result = timeout_at(
                deadline,
                self.core
                    .update(input, UpdateReason::Compaction, Some(through_sequence)),
            )
            .await;
            self.background.state.lock().unwrap().last_activity = Some(Instant::now());
            match result {
                Ok(UpdateOutcome::Committed | UpdateOutcome::Idle) => (),
                Ok(UpdateOutcome::Disabled) => {
                    // A settings revision may invalidate the attempted write
                    // while Wiki remains enabled; that is still a coverage gap.
                    ready = !self.core.host.memory_enabled();
                    break;
                }
                _ => break,
            }
        }
        if !ready {
            self.mark_pending(input.run_id);
        }
        // Retain deferred work after bounded catch-up, including while the main
        // conversation is idle. Newer queued snapshots win in schedule().
        self.schedule(input, UpdateReason::Compaction);
        ready
    }

    fn mark_pending(&self, run: &str) {
        self.core
            .host
            .memory_errors
            .write()
            .unwrap()
            .entry(run.to_owned())
            .or_insert_with(|| "压缩前记忆尚未补齐；原始记录已保留，会话仍可继续。".into());
    }
}

fn merge_reason(left: UpdateReason, right: UpdateReason) -> UpdateReason {
    match (left, right) {
        (UpdateReason::Compaction, _) | (_, UpdateReason::Compaction) => UpdateReason::Compaction,
        (UpdateReason::Response, _) | (_, UpdateReason::Response) => UpdateReason::Response,
        _ => UpdateReason::Progress,
    }
}

async fn worker<M: Model>(core: Arc<MemoryEngine<M>>, background: Arc<Background>, epoch: u64) {
    loop {
        let until = {
            let state = background.state.lock().unwrap();
            if state.epoch != epoch {
                return;
            }
            state
                .last_activity
                .map_or(Instant::now() + DEBOUNCE, |last| {
                    (last + BACKGROUND_INTERVAL).max(Instant::now() + DEBOUNCE)
                })
        };
        sleep_until(until).await;
        let mut pending = {
            let mut state = background.state.lock().unwrap();
            if state.epoch != epoch {
                return;
            }
            match state.pending.take() {
                Some(pending) => pending,
                None => {
                    state.worker.take();
                    return;
                }
            }
        };
        if core.host.memory_generation() != Some(pending.generation) {
            let mut state = background.state.lock().unwrap();
            if state.epoch != epoch {
                return;
            }
            if state.pending.is_none() {
                state.worker.take();
                return;
            }
            continue;
        }
        let outcome = core
            .update(pending.input.as_ref(), pending.reason, None)
            .await;
        let retry = match outcome {
            UpdateOutcome::Committed => {
                pending.reason = UpdateReason::Compaction;
                pending.failed_retries = 0;
                core.pending(pending.input.as_ref(), None).unwrap_or(false)
            }
            UpdateOutcome::Failed if pending.failed_retries == 0 => {
                pending.failed_retries += 1;
                true
            }
            UpdateOutcome::CoolingDown => true,
            _ => false,
        };
        let mut state = background.state.lock().unwrap();
        if state.epoch != epoch {
            return;
        }
        if !matches!(outcome, UpdateOutcome::Idle | UpdateOutcome::Disabled) {
            state.last_activity = Some(Instant::now());
        }
        if retry && core.host.memory_generation() == Some(pending.generation) {
            if let Some(newer) = &mut state.pending {
                newer.reason = merge_reason(newer.reason, pending.reason);
            } else {
                state.pending = Some(pending);
            }
        }
        if state.pending.is_none() {
            state.worker.take();
            return;
        }
    }
}
