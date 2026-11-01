use super::{AgentRuntime, AgentRuntimeError};
use crate::{ContextManager, EventStore, HistoryAccess, Model, ToolExecutor};
use agent_core::agent::{AgentRun, RunEvent};

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H>
where
    M: Model,
    T: ToolExecutor,
    S: EventStore,
    C: ContextManager,
    H: HistoryAccess<S>,
{
    /// Begin a queued turn while idle, with the same exclusion as direct input.
    pub async fn start_queued_turn(
        &self,
        run: &mut AgentRun,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        let _execution = self
            .executions
            .claim(run.id(), run.agent_id())
            .map_err(|_| AgentRuntimeError::RunAlreadyExecuting {
                run_id: run.id().to_owned(),
            })?;
        self.synchronize_run(run).await?;
        self.receive_user_inputs(run).await
    }

    /// The driver calls this at safe boundaries while it owns the execution.
    /// Delivery is an event, so a crash between append and acknowledgement cannot
    /// turn a queued correction into two user messages.
    pub(super) async fn receive_user_inputs(
        &self,
        run: &mut AgentRun,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        let Some(queue) = &self.user_inputs else {
            return Ok(false);
        };
        let mut delivered = false;
        while let Some(input) = queue
            .claim()
            .map_err(|e| AgentRuntimeError::EventStore(crate::EventStoreError::backend(e)))?
        {
            if let Some(saved) = run.events().iter().find(|entry| {
                matches!(
                    &entry.event, RunEvent::UserInputReceived { id, .. } if id == &input.id
                )
            }) {
                self.append_event(run.id(), run.agent_id(), saved).await?;
            } else {
                let interrupted_calls = run.interrupted_tool_calls();
                self.record_persisted(
                    run,
                    RunEvent::UserInputReceived {
                        id: input.id.clone(),
                        content: input.content,
                        files: input.files,
                        interrupted_calls,
                    },
                )
                .await?;
                delivered = true;
            }
            queue
                .acknowledge(&input.id)
                .map_err(|e| AgentRuntimeError::EventStore(crate::EventStoreError::backend(e)))?;
        }
        Ok(delivered)
    }

    pub(super) async fn wait_for_steering(&self) {
        let Some(queue) = &self.user_inputs else {
            return std::future::pending().await;
        };
        let mut changed = queue.subscribe();
        if changed.wait_for(|pending| *pending).await.is_err() {
            std::future::pending().await
        }
    }
}
