use agent_core::agent::{Agent, AgentRun, RunStatus};

use crate::{ContextManager, EventStore, HistoryAccess, Model, ToolExecutor};

use super::{AgentRuntime, AgentRuntimeError};

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H>
where
    M: Model,
    T: ToolExecutor,
    C: ContextManager,
    S: EventStore,
    H: HistoryAccess<S>,
{
    /// Compact settled history without running a model turn or changing the
    /// run's status. Returns false when there is nothing useful to compact.
    /// Admission shares the execution guard with model/tool work and input.
    pub async fn compact_now(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        if run.agent_id() != agent.id {
            return Err(AgentRuntimeError::AgentMismatch {
                expected_agent_id: agent.id.clone(),
                actual_agent_id: run.agent_id().into(),
            });
        }
        let execution = self
            .executions
            .claim_maintenance(run.id(), run.agent_id())
            .map_err(|_| AgentRuntimeError::RunAlreadyExecuting {
                run_id: run.id().into(),
            })?;
        self.synchronize_run(run).await?;
        if run.status() == RunStatus::Running {
            return Err(AgentRuntimeError::RunNotIdle {
                run_id: run.id().into(),
            });
        }
        let tools = self.tools.snapshot_definitions();
        self.validate_tool_definitions(&tools)?;
        let metadata = self.accounting_metadata();
        let result = execution
            .control
            .until_cancelled(|| {
                self.context_manager.compact_now(self.context_input(
                    agent,
                    run,
                    &tools,
                    metadata.as_ref(),
                ))
            })
            .await
            .ok_or(AgentRuntimeError::CompactionCancelled)?;
        result
            .map(|prepared| prepared.is_some())
            .map_err(|error| AgentRuntimeError::Context(Box::new(error)))
    }
}
