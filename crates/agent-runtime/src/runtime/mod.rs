mod driver;
mod execution;

use std::{error::Error, fmt};

use execution::ExecutionRegistry;

use agent_core::agent::{
    Agent, AgentRun, AgentRunError, AgentRunHistoryError, PreparedRunEvent, RunEvent,
    RunEventEnvelope, RunStatus,
};

use crate::{
    AppendOutcome, ContextManager, EventStore, EventStoreError, HistoryAccess, InMemoryEventStore,
    Model, NoHistoryAccess, RunEventNotification, RunEventReceiver, RunEventSubscriptions,
    ToolExecutor,
};

#[derive(Debug)]
pub struct AgentRuntime<M, T, C, S = InMemoryEventStore, H = NoHistoryAccess> {
    model: M,
    tools: T,
    context_manager: C,
    event_store: S,
    history_access: H,
    subscriptions: RunEventSubscriptions,
    max_steps: Option<u32>,
    executions: ExecutionRegistry,
}

impl<M, T, C> AgentRuntime<M, T, C, InMemoryEventStore, NoHistoryAccess> {
    pub fn new(model: M, tools: T, context_manager: C, max_steps: Option<u32>) -> Self {
        Self::with_event_store(
            model,
            tools,
            context_manager,
            InMemoryEventStore::new(),
            max_steps,
        )
    }
}

impl<M, T, C, S> AgentRuntime<M, T, C, S, NoHistoryAccess> {
    pub fn with_event_store(
        model: M,
        tools: T,
        context_manager: C,
        event_store: S,
        max_steps: Option<u32>,
    ) -> Self {
        Self {
            model,
            tools,
            context_manager,
            event_store,
            history_access: NoHistoryAccess,
            subscriptions: RunEventSubscriptions::default(),
            max_steps,
            executions: ExecutionRegistry::default(),
        }
    }
}

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H> {
    pub fn with_history_access<H2>(self, history_access: H2) -> AgentRuntime<M, T, C, S, H2> {
        AgentRuntime {
            model: self.model,
            tools: self.tools,
            context_manager: self.context_manager,
            event_store: self.event_store,
            history_access,
            subscriptions: self.subscriptions,
            max_steps: self.max_steps,
            executions: self.executions,
        }
    }

    pub fn model(&self) -> &M {
        &self.model
    }

    pub fn tools(&self) -> &T {
        &self.tools
    }

    pub fn event_store(&self) -> &S {
        &self.event_store
    }

    pub fn context_manager(&self) -> &C {
        &self.context_manager
    }

    pub fn context_manager_mut(&mut self) -> &mut C {
        &mut self.context_manager
    }

    pub fn history_access(&self) -> &H {
        &self.history_access
    }

    pub fn history_access_mut(&mut self) -> &mut H {
        &mut self.history_access
    }

    /// The configured step limit. `None` leaves the run without a policy limit.
    pub fn max_steps(&self) -> Option<u32> {
        self.max_steps
    }

    pub fn subscribe(&self) -> RunEventReceiver {
        self.subscriptions.subscribe()
    }

    /// Whether this runtime currently owns in-flight work for the run.
    /// This is independent of the durable `RunStatus::Running` state and does
    /// not report executions in other runtime instances or processes.
    pub fn is_executing(&self, run_id: &str) -> bool {
        self.executions.is_executing(run_id)
    }
}

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H>
where
    M: Model,
    T: ToolExecutor,
    S: EventStore,
    C: ContextManager,
    H: HistoryAccess<S>,
{
    pub async fn start_run(
        &self,
        agent: &Agent,
        run_id: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Result<AgentRun, AgentRuntimeError<M::Error>> {
        let run = agent.start_run(run_id, instruction);
        let event = run
            .events()
            .first()
            .expect("a new run always contains RunStarted")
            .clone();

        self.append_event(run.id(), run.agent_id(), &event).await?;
        self.publish(&run, event);

        Ok(run)
    }

    pub async fn restore_run(
        &self,
        run_id: &str,
    ) -> Result<Option<AgentRun>, AgentRuntimeError<M::Error>> {
        let Some(stored) = self.event_store.load(run_id).await? else {
            return Ok(None);
        };

        AgentRun::restore(stored.run_id, stored.agent_id, stored.events)
            .map(Some)
            .map_err(AgentRuntimeError::History)
    }

    pub async fn replay_events(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
    ) -> Result<Vec<RunEventNotification>, AgentRuntimeError<M::Error>> {
        let Some(stored) = self.event_store.load(run_id).await? else {
            return Ok(Vec::new());
        };

        let restored = AgentRun::restore(stored.run_id, stored.agent_id, stored.events)?;

        Ok(restored
            .events()
            .iter()
            .filter(|event| after_sequence.is_none_or(|sequence| event.sequence > sequence))
            .map(|envelope| RunEventNotification {
                run_id: restored.id().to_owned(),
                agent_id: restored.agent_id().to_owned(),
                envelope: envelope.clone(),
            })
            .collect())
    }

    /// Subscribes to this run's live notifications and returns persisted catch-up events.
    ///
    /// An event committed during setup can appear in both results. Consumers
    /// should de-duplicate by `(run_id, sequence)`.
    pub async fn subscribe_from(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
    ) -> Result<(Vec<RunEventNotification>, RunEventReceiver), AgentRuntimeError<M::Error>> {
        let receiver = self.subscriptions.subscribe_run(run_id);
        let persisted = self.replay_events(run_id, after_sequence).await?;
        Ok((persisted, receiver))
    }

    pub async fn record_event(
        &self,
        run: &mut AgentRun,
        event: RunEvent,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        if matches!(event, RunEvent::RunCancelled) {
            return self.cancel_run(run).await;
        }
        let _execution = self
            .executions
            .claim(run.id(), run.agent_id())
            .map_err(|_| AgentRuntimeError::RunAlreadyExecuting {
                run_id: run.id().to_owned(),
            })?;
        self.synchronize_run(run).await?;
        self.record_persisted(run, event).await
    }

    pub async fn submit_input(
        &self,
        run: &mut AgentRun,
        content: impl Into<String>,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        self.record_event(
            run,
            RunEvent::InputReceived {
                content: content.into(),
            },
        )
        .await
    }

    /// Stops this runtime's active execution before acknowledging cancellation.
    ///
    /// In-flight model/tool futures are dropped at a cancellation boundary;
    /// persistence is allowed to finish. Adapters must clean up their owned
    /// work when dropped. An external side effect already committed cannot be
    /// rolled back by cancellation. Use the same runtime to execute and cancel.
    pub async fn cancel_run(&self, run: &mut AgentRun) -> Result<(), AgentRuntimeError<M::Error>> {
        let _execution = loop {
            match self.executions.claim(run.id(), run.agent_id()) {
                Ok(execution) => break execution,
                Err(control) => {
                    if control.agent_id() != run.agent_id() {
                        return Err(AgentRuntimeError::AgentMismatch {
                            expected_agent_id: control.agent_id().to_owned(),
                            actual_agent_id: run.agent_id().to_owned(),
                        });
                    }
                    control.request_cancellation();
                    control.finished().await;
                }
            }
        };
        // A caller may hold the snapshot from before the active task advanced.
        // Refresh only after that task has stopped writing.
        if let Some(restored) = self.restore_run(run.id()).await? {
            if restored.agent_id() != run.agent_id() {
                return Err(AgentRuntimeError::AgentMismatch {
                    expected_agent_id: run.agent_id().to_owned(),
                    actual_agent_id: restored.agent_id().to_owned(),
                });
            }
            *run = restored;
        } else {
            self.synchronize_run(run).await?;
        }
        if run.status() == RunStatus::Cancelled {
            // The worker may have written the terminal record and then received
            // an uncertain durability error. Reading it back is not an ack;
            // retry the exact envelope so the store confirms durability.
            self.append_event(
                run.id(),
                run.agent_id(),
                run.events()
                    .last()
                    .expect("a cancelled run has a terminal event"),
            )
            .await?;
            return Ok(());
        }
        self.record_persisted(run, RunEvent::RunCancelled).await
    }

    async fn synchronize_run(&self, run: &AgentRun) -> Result<(), AgentRuntimeError<M::Error>> {
        let Some(stored) = self.event_store.load(run.id()).await? else {
            for event in run.events() {
                self.append_event(run.id(), run.agent_id(), event).await?;
                self.publish(run, event.clone());
            }
            return Ok(());
        };

        let stored = AgentRun::restore(stored.run_id, stored.agent_id, stored.events)?;

        if stored.agent_id() != run.agent_id() {
            return Err(EventStoreError::AgentMismatch {
                run_id: run.id().to_owned(),
                expected: stored.agent_id().to_owned(),
                actual: run.agent_id().to_owned(),
            }
            .into());
        }

        if stored.events().len() > run.events().len() {
            return Err(AgentRuntimeError::RunOutOfDate {
                run_id: run.id().to_owned(),
                stored_events: stored.events().len(),
                memory_events: run.events().len(),
            });
        }

        for (index, stored_event) in stored.events().iter().enumerate() {
            if stored_event != &run.events()[index] {
                return Err(AgentRuntimeError::RunHistoryConflict {
                    run_id: run.id().to_owned(),
                    sequence: stored_event.sequence,
                });
            }
        }

        for event in &run.events()[stored.events().len()..] {
            self.append_event(run.id(), run.agent_id(), event).await?;
            self.publish(run, event.clone());
        }

        Ok(())
    }

    async fn begin_step_persisted(
        &self,
        run: &mut AgentRun,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let candidate = run.prepare_begin_step()?;
        self.commit_candidate(run, candidate).await
    }

    async fn record_persisted(
        &self,
        run: &mut AgentRun,
        event: RunEvent,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let candidate = run.prepare_event(event)?;
        self.commit_candidate(run, candidate).await
    }

    async fn commit_candidate(
        &self,
        run: &mut AgentRun,
        candidate: PreparedRunEvent,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let event = candidate.envelope().clone();

        self.append_event(run.id(), run.agent_id(), &event).await?;
        run.commit_prepared(candidate)?;
        self.publish(run, event);

        Ok(())
    }

    async fn append_event(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, AgentRuntimeError<M::Error>> {
        self.event_store
            .append(run_id, agent_id, event)
            .await
            .map_err(AgentRuntimeError::EventStore)
    }

    fn publish(&self, run: &AgentRun, envelope: RunEventEnvelope) {
        self.subscriptions.publish(RunEventNotification {
            run_id: run.id().to_owned(),
            agent_id: run.agent_id().to_owned(),
            envelope,
        });
    }
}

#[derive(Debug)]
pub enum AgentRuntimeError<ModelError> {
    RunAlreadyExecuting {
        run_id: String,
    },
    AgentMismatch {
        expected_agent_id: String,
        actual_agent_id: String,
    },
    Model(ModelError),
    ModelStream(crate::ModelStreamError),
    ModelProtocol(crate::ModelProtocolError),
    ModelCapabilities(crate::ModelCapabilitiesError),
    OutputTruncated,
    Context(Box<dyn Error + Send + Sync>),
    ProviderContextMismatch,
    ProviderReplay(crate::ProviderReplayError),
    AgentRun(AgentRunError),
    History(AgentRunHistoryError),
    EventStore(EventStoreError),
    InterruptedRun {
        run_id: String,
        last_sequence: u64,
    },
    RunOutOfDate {
        run_id: String,
        stored_events: usize,
        memory_events: usize,
    },
    RunHistoryConflict {
        run_id: String,
        sequence: u64,
    },
    DuplicateToolName {
        name: String,
    },
    DuplicateToolCallId {
        call_id: String,
    },
    EmptyToolCalls,
    StepLimitExceeded {
        max_steps: u32,
    },
}

impl<ModelError> fmt::Display for AgentRuntimeError<ModelError>
where
    ModelError: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunAlreadyExecuting { run_id } => {
                write!(formatter, "run {run_id} already has an active execution")
            }
            Self::AgentMismatch {
                expected_agent_id,
                actual_agent_id,
            } => write!(
                formatter,
                "run belongs to agent {actual_agent_id}, not agent {expected_agent_id}"
            ),
            Self::Model(error) => write!(formatter, "model request failed: {error}"),
            Self::ModelStream(error) => write!(formatter, "invalid model stream: {error}"),
            Self::ModelProtocol(error) => write!(formatter, "invalid model content: {error}"),
            Self::ModelCapabilities(error) => {
                write!(formatter, "invalid model capabilities: {error}")
            }
            Self::OutputTruncated => {
                formatter.write_str("model output was truncated before completion")
            }
            Self::Context(error) => write!(formatter, "context management failed: {error}"),
            Self::ProviderReplay(error) => write!(formatter, "provider replay failed: {error}"),
            Self::ProviderContextMismatch => formatter
                .write_str("model does not support the prepared provider/model/context format"),
            Self::AgentRun(error) => write!(formatter, "invalid agent run transition: {error}"),
            Self::History(error) => write!(formatter, "invalid stored run history: {error}"),
            Self::EventStore(error) => write!(formatter, "event store failed: {error}"),
            Self::InterruptedRun {
                run_id,
                last_sequence,
            } => write!(
                formatter,
                "run {run_id} was interrupted after event {last_sequence}; explicitly cancel it before starting a new run"
            ),
            Self::RunOutOfDate {
                run_id,
                stored_events,
                memory_events,
            } => write!(
                formatter,
                "run {run_id} is out of date: storage has {stored_events} events, memory has {memory_events}"
            ),
            Self::RunHistoryConflict { run_id, sequence } => write!(
                formatter,
                "run {run_id} differs from stored history at sequence {sequence}"
            ),
            Self::DuplicateToolName { name } => {
                write!(formatter, "more than one tool is named {name:?}")
            }
            Self::DuplicateToolCallId { call_id } => {
                write!(formatter, "model reused tool call id {call_id:?}")
            }
            Self::EmptyToolCalls => formatter.write_str("model returned an empty tool-call list"),
            Self::StepLimitExceeded { max_steps } => {
                write!(formatter, "maximum number of steps ({max_steps}) reached")
            }
        }
    }
}

impl<ModelError> Error for AgentRuntimeError<ModelError>
where
    ModelError: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(error) => Some(error),
            Self::ModelStream(error) => Some(error),
            Self::ModelProtocol(error) => Some(error),
            Self::ModelCapabilities(error) => Some(error),
            Self::ProviderReplay(error) => Some(error),
            Self::Context(error) => Some(error.as_ref()),
            Self::AgentRun(error) => Some(error),
            Self::History(error) => Some(error),
            Self::EventStore(error) => Some(error),
            Self::RunAlreadyExecuting { .. }
            | Self::OutputTruncated
            | Self::ProviderContextMismatch
            | Self::AgentMismatch { .. }
            | Self::InterruptedRun { .. }
            | Self::RunOutOfDate { .. }
            | Self::RunHistoryConflict { .. }
            | Self::DuplicateToolName { .. }
            | Self::DuplicateToolCallId { .. }
            | Self::EmptyToolCalls
            | Self::StepLimitExceeded { .. } => None,
        }
    }
}

impl<ModelError> From<AgentRunError> for AgentRuntimeError<ModelError> {
    fn from(error: AgentRunError) -> Self {
        Self::AgentRun(error)
    }
}

impl<ModelError> From<AgentRunHistoryError> for AgentRuntimeError<ModelError> {
    fn from(error: AgentRunHistoryError) -> Self {
        Self::History(error)
    }
}

impl<ModelError> From<EventStoreError> for AgentRuntimeError<ModelError> {
    fn from(error: EventStoreError) -> Self {
        Self::EventStore(error)
    }
}
