use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

use tokio::sync::watch;

/// Coordinates execution within one runtime instance. Storage remains the
/// authority for durable run state; this registry only tracks in-flight work.
#[derive(Debug, Default)]
pub(super) struct ExecutionRegistry {
    runs: Arc<Mutex<HashMap<String, Arc<RunControl>>>>,
}

impl ExecutionRegistry {
    pub(super) fn is_executing(&self, run_id: &str) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(run_id)
    }

    pub(super) fn claim(
        &self,
        run_id: &str,
        agent_id: &str,
    ) -> Result<ExecutionGuard, Arc<RunControl>> {
        self.claim_kind(run_id, agent_id, false)
    }

    pub(super) fn claim_maintenance(
        &self,
        run_id: &str,
        agent_id: &str,
    ) -> Result<ExecutionGuard, Arc<RunControl>> {
        self.claim_kind(run_id, agent_id, true)
    }

    pub(super) fn maintenance(&self, run_id: &str) -> Option<Arc<RunControl>> {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(run_id)
            .filter(|control| control.maintenance)
            .cloned()
    }

    fn claim_kind(
        &self,
        run_id: &str,
        agent_id: &str,
        maintenance: bool,
    ) -> Result<ExecutionGuard, Arc<RunControl>> {
        let mut runs = self.runs.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(control) = runs.get(run_id) {
            return Err(Arc::clone(control));
        }
        let control = Arc::new(RunControl::new(agent_id, maintenance));
        runs.insert(run_id.to_owned(), Arc::clone(&control));
        Ok(ExecutionGuard {
            run_id: run_id.to_owned(),
            control,
            runs: Arc::clone(&self.runs),
        })
    }
}

#[derive(Debug)]
pub(super) struct RunControl {
    agent_id: String,
    maintenance: bool,
    cancelled: watch::Sender<bool>,
    finished: watch::Sender<bool>,
}

impl RunControl {
    fn new(agent_id: &str, maintenance: bool) -> Self {
        Self {
            agent_id: agent_id.to_owned(),
            maintenance,
            cancelled: watch::channel(false).0,
            finished: watch::channel(false).0,
        }
    }

    pub(super) fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub(super) fn request_cancellation(&self) {
        self.cancelled.send_replace(true);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.cancelled.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    pub(super) async fn finished(&self) {
        let mut receiver = self.finished.subscribe();
        let _ = receiver.wait_for(|finished| *finished).await;
    }

    // Poll cancellation first, and construct provider futures lazily: even
    // synchronous work in execute()/respond() must not start after cancellation.
    pub(super) async fn until_cancelled<F, Fut>(&self, operation: F) -> Option<Fut::Output>
    where
        F: FnOnce() -> Fut,
        Fut: Future,
    {
        tokio::select! {
            biased;
            _ = self.cancelled() => None,
            result = async { operation().await } => Some(result),
        }
    }
}

pub(super) struct ExecutionGuard {
    run_id: String,
    pub(super) control: Arc<RunControl>,
    runs: Arc<Mutex<HashMap<String, Arc<RunControl>>>>,
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        let mut runs = self.runs.lock().unwrap_or_else(|error| error.into_inner());
        runs.remove(&self.run_id);
        self.control.finished.send_replace(true);
    }
}
