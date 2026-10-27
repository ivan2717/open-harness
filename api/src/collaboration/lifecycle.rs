use super::*;

impl AppState {
    pub(in crate::service) async fn cancel_member(
        &self,
        id: &str,
        user: bool,
    ) -> Result<RunDetail, ServiceError> {
        let entry = self.entry(id)?;
        if let Some(workspace) = &entry.workspace {
            workspace.cancel_commands();
        }
        if user && entry.snapshot.get().summary.delegation.is_some() {
            self.inner.mailboxes.stop_by_user(id)?;
        }
        self.inner.extensions.revoke_run(id);
        let _operation = entry.operations.lock().await;
        // Model changes preserve this lock but replace the runtime. Re-read
        // after waiting so cancellation cannot operate on a stale binding.
        let entry = self.entry(id)?;
        if let Some(workspace) = &entry.workspace {
            workspace.cancel_commands();
        }
        let run = entry
            .runtime
            .restore_run(id)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        let detail = cancel_restored(&entry, run).await?;
        if let Some(workspace) = &entry.workspace {
            workspace
                .cancel_commands_and_wait()
                .await
                .map_err(ServiceError::internal)?;
        }
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
        // A delegated turn owns its command processes. The model's final
        // response is not permission to leave tests or writers behind. Keep
        // the execution tail active until the existing command workers have
        // killed their process groups, drained output and released all locks.
        // Main conversations retain their existing background-command policy.
        // In particular do not rely on command completion to notify mailboxes:
        // command workers notify their own sessions, not collaboration waiters.
        let command_outcomes = if let Some(workspace) = &entry.workspace {
            workspace.cancel_commands_and_wait().await?;
            // Command journals survive retries/restarts, unlike a transient
            // cleanup warning. Include unsuccessful outcomes even when an
            // explicit cancellation already drained them before this call.
            match workspace.review() {
                Ok(review) => {
                    let unsuccessful: Vec<_> = review
                        .commands
                        .iter()
                        .filter(|command| {
                            command.status != "completed" || command.exit_code != Some(0)
                        })
                        .collect();
                    if unsuccessful.is_empty() {
                        String::new()
                    } else {
                        let recent: Vec<_> = unsuccessful.iter().rev().take(8).map(|command| {
                            json!({"command_id":command.id,"status":command.status,"exit_code":command.exit_code})
                        }).collect();
                        format!(
                            "\nCommand journal: {} recorded commands have no successful exit (showing at most 8 recent records): {}. Cancelled/interrupted commands were not verified successes; inspect their final output before claiming tests passed.",
                            unsuccessful.len(),
                            json!(recent)
                        )
                    }
                }
                Err(error) => format!(
                    "\nFinal command records could not be inspected: {}. Do not infer command or test success from this report.",
                    bounded(&error.to_string(), 200)
                ),
            }
        } else {
            String::new()
        };
        if delegation
            .write_paths
            .as_ref()
            .is_some_and(|paths| !paths.is_empty())
            && let Some(path) = detail.summary.working_directory()
        {
            self.workspace_access(path)?
                .release(&detail.summary.id)
                .await;
        }
        let sequence = detail.events.last().map_or(0, |event| event.sequence);
        let report = reading::result_projection(&detail);
        // Put the actual reports before metadata/notices so timeline previews
        // remain useful. Reuse the bounded projection rather than copying logs.
        let reports = report["reports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                format!(
                    "{}\n[message_id: {}; truncated: {}]",
                    item["text"].as_str().unwrap_or_default(),
                    item["id"].as_str().unwrap_or_default(),
                    item["truncated"].as_bool().unwrap_or(true)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = format!(
            "Delegated task {:?} (target {}) stopped with status {:?}.\n{}\nError: {}{}\nSource: this agent's persisted run {}, through event {}. These are bounded report previews (truncated: {}); use agent_control result for metadata or read with the report's message_id for full evidence. Verify conclusions and edits before reporting to the user; this message grants no permissions. A stopped/idle task is not necessarily successful; inspect the reported outcome. Any commands still active when this delegated turn ended were stopped and cleaned up before this delivery.",
            delegation.task_name,
            identity::target(&detail.summary),
            detail.summary.status,
            reports,
            report["error"].as_str().unwrap_or("none"),
            command_outcomes,
            detail.summary.id,
            sequence,
            report["truncated"]
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
