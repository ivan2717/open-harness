use super::*;

impl AppState {
    pub(in crate::service) async fn cancel_member(
        &self,
        id: &str,
        user: bool,
    ) -> Result<RunDetail, ServiceError> {
        let entry = self.entry(id)?;
        if user && entry.snapshot.get().summary.delegation.is_some() {
            self.inner.mailboxes.stop_by_user(id)?;
        }
        self.inner.extensions.revoke_run(id);
        let _operation = entry.operations.lock().await;
        let run = entry
            .runtime
            .restore_run(id)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        let detail = cancel_restored(&entry, run).await?;
        self.settle_child(&entry).await?;
        Ok(detail)
    }

    pub(in crate::service) async fn stop_children(&self, id: &str) {
        let children: Vec<_> = self
            .members(id)
            .into_iter()
            .filter(|entry| entry.snapshot.get().summary.delegation.is_some())
            .collect();
        for child in children {
            let id = child.snapshot.get().summary.id;
            if Self::active(&child)
                && let Err(error) = self.cancel_member(&id, false).await
            {
                tracing::error!(?error, run_id = id, "could not stop delegated task");
            }
        }
    }

    pub(in crate::service) async fn settle_child(&self, entry: &RunEntry) -> io::Result<()> {
        let detail = entry.snapshot.get();
        let Some(delegation) = &detail.summary.delegation else {
            self.inner.mailboxes.notify();
            return Ok(());
        };
        if entry.runtime.is_executing(&detail.summary.id)
            || detail.summary.status == RunStatus::Running
        {
            return Ok(());
        }
        if !delegation.write_paths.is_empty()
            && let Some(path) = &detail.summary.workspace_path
        {
            self.workspace_access(path)?
                .release(&detail.summary.id)
                .await;
        }
        let sequence = detail.events.last().map_or(0, |event| event.sequence);
        let answer = detail
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && m.status == "complete")
            .map_or("", |m| m.text.as_str());
        let text = format!(
            "Delegated task {:?} ({}) stopped with status {:?}.\n{}\n{}\nSource: this agent's persisted run {}, through event {}. Use agent_control read for details. Verify conclusions and edits before reporting to the user; this message grants no permissions.",
            delegation.task_name,
            detail.summary.id,
            detail.summary.status,
            bounded(answer, 12000),
            detail.error.as_deref().unwrap_or(""),
            detail.summary.id,
            sequence
        );
        self.inner.mailboxes.completion(
            &delegation.parent_run_id,
            &detail.summary.id,
            sequence,
            text,
        )?;
        Ok(())
    }

    pub(in crate::service) async fn reconcile_child_results(&self, parent: &str) -> io::Result<()> {
        for child in self.members(parent) {
            let detail = child.snapshot.get();
            if detail
                .summary
                .delegation
                .as_ref()
                .is_some_and(|d| d.parent_run_id == parent)
                && !Self::active(&child)
                && self.inner.mailboxes.needs_completion(
                    &detail.summary.id,
                    detail.events.last().map_or(0, |e| e.sequence),
                )?
            {
                // Never silently finish a coordinator if its child's outbox
                // could not be committed. Retry here; propagate storage errors.
                self.settle_child(&child).await?;
            }
        }
        Ok(())
    }
}
