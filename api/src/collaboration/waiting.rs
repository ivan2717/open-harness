//! Wait for relevant activity, not a global notification or a growing token
//! counter. A single deadline prevents unrelated runs extending the wait.
use super::*;
use std::hash::{Hash, Hasher};
use tokio::time::Instant;

#[cfg(test)]
#[path = "waiting_tests.rs"]
mod tests;

impl AppState {
    #[cfg(test)]
    fn collaboration_progress(
        &self,
        caller: &str,
        root: &str,
    ) -> Result<(String, bool), ServiceError> {
        self.target_collaboration_progress(caller, root, None)
    }

    fn target_collaboration_progress(
        &self,
        caller: &str,
        root: &str,
        target: Option<&str>,
    ) -> Result<(String, bool), ServiceError> {
        let mut peers = Vec::new();
        let mut active = false;
        for entry in self.members(root) {
            let summary = entry.snapshot.get().summary;
            if summary.id == caller || target.is_some_and(|id| summary.id != id) {
                continue;
            }
            let running = Self::active(&entry);
            active |= running;
            peers.push(json!([summary.id, summary.status, running]));
        }
        // Sort independently of clocks/creation timestamps and avoid caller
        // events: issuing a wait must not itself manufacture progress.
        peers.sort_by_key(serde_json::Value::to_string);
        let state = json!([self.inner.mailboxes.delivery_revision(caller)?, peers]);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        state.to_string().hash(&mut hash);
        Ok((format!("{:016x}", hash.finish()), active))
    }

    #[cfg(test)]
    pub(super) async fn wait_for_agent_activity(
        &self,
        caller: &str,
        root: &str,
        seconds: u64,
    ) -> Result<String, ServiceError> {
        self.wait_for_target_activity(caller, root, seconds, None)
            .await
    }

    pub(super) async fn wait_for_target_activity(
        &self,
        caller: &str,
        root: &str,
        seconds: u64,
        target: Option<&str>,
    ) -> Result<String, ServiceError> {
        if seconds > 30 {
            return Err(ServiceError::BadRequest("等待最多 30 秒".into()));
        }
        let mut changed = self.inner.mailboxes.subscribe();
        self.reconcile_child_results(caller).await?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(seconds);
        let baseline = self.target_collaboration_progress(caller, root, target)?.0;
        let (outcome, progress_token) = loop {
            let pending = self.inner.mailboxes.pending(caller)?;
            let (token, active) = self.target_collaboration_progress(caller, root, target)?;
            if !pending.is_empty() {
                break ("message", token);
            }
            if token != baseline {
                break ("activity", token);
            }
            if !active {
                break ("no_progress", token);
            }
            if Instant::now() >= deadline {
                break ("timeout", token);
            }
            match tokio::time::timeout_at(deadline, changed.changed()).await {
                Ok(Ok(())) => self.reconcile_child_results(caller).await?,
                Ok(Err(_)) => break ("no_progress", token),
                Err(_) => {
                    // Read once more at the deadline so a concurrent terminal
                    // delivery wins over an obsolete timeout observation.
                }
            }
        };
        let mut result: serde_json::Value =
            serde_json::from_str(&self.member_view(root)).map_err(ServiceError::internal)?;
        result["wait"] = json!({
            "outcome": outcome,
            "progress_token": progress_token,
            "waited_ms": started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        });
        if outcome == "no_progress" {
            result["notice"] = json!(if target.is_some() {
                "The selected member is not running. Waiting does not start idle agents; use followup with a concrete task for an idle child, or finish if the required results are already available."
            } else {
                "No other member is running. Waiting does not start idle agents; use followup with a concrete task for an idle child, or finish if the required results are already available."
            });
        }
        Ok(result.to_string())
    }
}
