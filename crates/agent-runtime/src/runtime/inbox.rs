use std::collections::HashSet;

use agent_core::agent::{AgentRun, RunEvent};

use crate::{ContextManager, EventStore, HistoryAccess, Model, ToolExecutor};

use super::{AgentRuntime, AgentRuntimeError, execution::RunControl};

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H>
where
    M: Model,
    T: ToolExecutor,
    S: EventStore,
    C: ContextManager,
    H: HistoryAccess<S>,
{
    /// Consume inbox entries only after their durable delivery event exists.
    /// Re-delivery after a crash is acknowledged without duplicating context.
    pub(super) async fn receive_agent_messages(
        &self,
        run: &mut AgentRun,
        control: &RunControl,
        wait: bool,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        let mut acknowledged_ids = HashSet::new();
        loop {
            let result = control
                .until_cancelled(|| async {
                    if wait {
                        self.tools.wait_for_messages().await
                    } else {
                        self.tools.pending_messages().await
                    }
                })
                .await;
            let Some(result) = result else {
                self.record_persisted(run, RunEvent::RunCancelled).await?;
                return Ok(false);
            };
            let messages = match result {
                Ok(messages) => messages,
                Err(error) => return self.coordination_failed(run, error.to_string()).await,
            };
            if messages.is_empty() {
                return Ok(false);
            }
            if wait
                && messages
                    .iter()
                    .all(|message| acknowledged_ids.contains(&message.id))
            {
                return self
                    .coordination_failed(
                        run,
                        "message inbox returned only already acknowledged entries without making progress"
                            .into(),
                    )
                    .await;
            }
            let mut delivered = false;
            for message in messages {
                let id = message.id.clone();
                if !run.has_agent_message(&id) {
                    self.record_persisted(run, RunEvent::AgentMessageReceived { message })
                        .await?;
                    delivered = true;
                } else {
                    let saved = run
                        .events()
                        .iter()
                        .find(|entry| {
                            matches!(
                                &entry.event,
                                RunEvent::AgentMessageReceived { message } if message.id == id
                            )
                        })
                        .expect("message ID index matches the durable history");
                    if !matches!(&saved.event, RunEvent::AgentMessageReceived { message: existing } if existing == &message)
                    {
                        return self
                            .coordination_failed(
                                run,
                                format!("message ID {id:?} was reused with different content"),
                            )
                            .await;
                    }
                    // Reading back an event after an uncertain append is not itself
                    // a durability acknowledgement. Retry its exact envelope first.
                    self.append_event(run.id(), run.agent_id(), saved).await?;
                }
                let acknowledged = control
                    .until_cancelled(|| self.tools.acknowledge_messages(std::slice::from_ref(&id)))
                    .await;
                let Some(acknowledged) = acknowledged else {
                    self.record_persisted(run, RunEvent::RunCancelled).await?;
                    return Ok(false);
                };
                if let Err(error) = acknowledged {
                    return self.coordination_failed(run, error.to_string()).await;
                }
                acknowledged_ids.insert(id);
            }
            if delivered || !wait {
                return Ok(delivered);
            }
            // A replayed batch is not confirmation that all owned work is done.
            // Ask again after acknowledgement, while giving cancellation a chance
            // even when a host hook returns immediately without yielding.
            tokio::task::yield_now().await;
        }
    }

    async fn coordination_failed(
        &self,
        run: &mut AgentRun,
        message: String,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        let error = AgentRuntimeError::Coordination(message);
        self.record_persisted(
            run,
            RunEvent::RunFailed {
                message: error.to_string(),
            },
        )
        .await?;
        Err(error)
    }
}
