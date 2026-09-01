use std::{error::Error, fmt};

use agent_core::agent::{
    Agent, AgentRun, AgentRunError, AgentRunHistoryError, RunEvent, RunEventEnvelope, RunStatus,
};

use crate::{
    AppendOutcome, EventStore, EventStoreError, InMemoryEventStore, Model, ModelMessage,
    ModelRequest, ModelResponse, RunEventNotification, RunEventReceiver, RunEventSubscriptions,
    ToolDefinition, ToolExecutor,
};

#[derive(Debug)]
pub struct AgentRuntime<M, T, S = InMemoryEventStore> {
    model: M,
    tools: T,
    event_store: S,
    subscriptions: RunEventSubscriptions,
    max_steps: u32,
}

impl<M, T> AgentRuntime<M, T, InMemoryEventStore> {
    pub fn new(model: M, tools: T, max_steps: Option<u32>) -> Self {
        Self::with_event_store(model, tools, max_steps, InMemoryEventStore::new())
    }
}

impl<M, T, S> AgentRuntime<M, T, S> {
    pub fn with_event_store(model: M, tools: T, max_steps: Option<u32>, event_store: S) -> Self {
        Self {
            model,
            tools,
            event_store,
            subscriptions: RunEventSubscriptions::default(),
            max_steps: max_steps.unwrap_or(i32::MAX as u32),
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

    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    pub fn subscribe(&self) -> RunEventReceiver {
        self.subscriptions.subscribe()
    }
}

impl<M, T, S> AgentRuntime<M, T, S>
where
    M: Model,
    T: ToolExecutor,
    S: EventStore,
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

        AgentRun::restore(
            stored.run_id.clone(),
            stored.agent_id.clone(),
            stored.events.clone(),
        )?;

        Ok(stored
            .events
            .into_iter()
            .filter(|event| after_sequence.is_none_or(|sequence| event.sequence > sequence))
            .map(|envelope| RunEventNotification {
                run_id: stored.run_id.clone(),
                agent_id: stored.agent_id.clone(),
                envelope,
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

    pub async fn cancel_run(&self, run: &mut AgentRun) -> Result<(), AgentRuntimeError<M::Error>> {
        self.record_event(run, RunEvent::RunCancelled).await
    }

    pub async fn run_until_blocked(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
    ) -> Result<RunStatus, AgentRuntimeError<M::Error>> {
        if run.agent_id() != agent.id {
            return Err(AgentRuntimeError::AgentMismatch {
                expected_agent_id: agent.id.clone(),
                actual_agent_id: run.agent_id().to_owned(),
            });
        }

        self.synchronize_run(run).await?;
        self.ensure_safe_execution_entry(run)?;

        while run.is_running() {
            if run.current_step() >= self.max_steps {
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: format!("maximum number of steps ({}) reached", self.max_steps),
                    },
                )
                .await?;

                return Err(AgentRuntimeError::StepLimitExceeded {
                    max_steps: self.max_steps,
                });
            }

            self.begin_step_persisted(run).await?;

            let request = build_model_request(agent, run, self.tools.definitions());
            let response = match self.model.respond(request).await {
                Ok(response) => response,
                Err(source) => {
                    self.record_persisted(
                        run,
                        RunEvent::RunFailed {
                            message: source.to_string(),
                        },
                    )
                    .await?;

                    return Err(AgentRuntimeError::Model(source));
                }
            };

            match response {
                ModelResponse::FinalAnswer { answer } => {
                    self.record_persisted(
                        run,
                        RunEvent::ModelResponseReceived {
                            content: answer.clone(),
                        },
                    )
                    .await?;
                    self.record_persisted(run, RunEvent::RunCompleted { answer })
                        .await?;
                }
                ModelResponse::ToolCalls { content, calls } => {
                    self.record_persisted(run, RunEvent::ModelResponseReceived { content })
                        .await?;

                    if calls.is_empty() {
                        self.record_persisted(
                            run,
                            RunEvent::RunFailed {
                                message: "model returned an empty tool-call list".to_owned(),
                            },
                        )
                        .await?;

                        return Err(AgentRuntimeError::EmptyToolCalls);
                    }

                    for call in &calls {
                        self.record_persisted(
                            run,
                            RunEvent::ToolCallRequested {
                                call_id: call.id.clone(),
                                tool_name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        )
                        .await?;
                    }

                    for call in calls {
                        let call_id = call.id.clone();

                        match self.tools.execute(call).await {
                            Ok(output) => {
                                self.record_persisted(
                                    run,
                                    RunEvent::ToolCallSucceeded { call_id, output },
                                )
                                .await?;
                            }
                            Err(error) => {
                                self.record_persisted(
                                    run,
                                    RunEvent::ToolCallFailed {
                                        call_id,
                                        message: error.to_string(),
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                }
                ModelResponse::InputRequired { prompt } => {
                    self.record_persisted(
                        run,
                        RunEvent::ModelResponseReceived {
                            content: prompt.clone(),
                        },
                    )
                    .await?;
                    self.record_persisted(run, RunEvent::InputRequested { prompt })
                        .await?;
                }
            }
        }

        Ok(run.status())
    }

    fn ensure_safe_execution_entry(
        &self,
        run: &AgentRun,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        if !run.is_running() {
            return Ok(());
        }

        let last = run
            .events()
            .last()
            .expect("an AgentRun always contains RunStarted");
        if matches!(
            last.event,
            RunEvent::RunStarted { .. } | RunEvent::InputReceived { .. }
        ) {
            return Ok(());
        }

        Err(AgentRuntimeError::InterruptedRun {
            run_id: run.id().to_owned(),
            last_sequence: last.sequence,
        })
    }

    async fn synchronize_run(&self, run: &AgentRun) -> Result<(), AgentRuntimeError<M::Error>> {
        let Some(stored) = self.event_store.load(run.id()).await? else {
            for event in run.events() {
                self.append_event(run.id(), run.agent_id(), event).await?;
                self.publish(run, event.clone());
            }
            return Ok(());
        };

        AgentRun::restore(
            stored.run_id.clone(),
            stored.agent_id.clone(),
            stored.events.clone(),
        )?;

        if stored.agent_id != run.agent_id() {
            return Err(EventStoreError::AgentMismatch {
                run_id: run.id().to_owned(),
                expected: stored.agent_id,
                actual: run.agent_id().to_owned(),
            }
            .into());
        }

        if stored.events.len() > run.events().len() {
            return Err(AgentRuntimeError::RunOutOfDate {
                run_id: run.id().to_owned(),
                stored_events: stored.events.len(),
                memory_events: run.events().len(),
            });
        }

        for (index, stored_event) in stored.events.iter().enumerate() {
            if stored_event != &run.events()[index] {
                return Err(AgentRuntimeError::RunHistoryConflict {
                    run_id: run.id().to_owned(),
                    sequence: stored_event.sequence,
                });
            }
        }

        for event in &run.events()[stored.events.len()..] {
            self.append_event(run.id(), run.agent_id(), event).await?;
            self.publish(run, event.clone());
        }

        Ok(())
    }

    async fn begin_step_persisted(
        &self,
        run: &mut AgentRun,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let mut candidate = run.clone();
        candidate.begin_step()?;
        self.commit_candidate(run, candidate).await
    }

    async fn record_persisted(
        &self,
        run: &mut AgentRun,
        event: RunEvent,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let mut candidate = run.clone();
        candidate.record(event)?;
        self.commit_candidate(run, candidate).await
    }

    async fn commit_candidate(
        &self,
        run: &mut AgentRun,
        candidate: AgentRun,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let event = candidate
            .events()
            .last()
            .expect("the candidate contains the newly recorded event")
            .clone();

        self.append_event(candidate.id(), candidate.agent_id(), &event)
            .await?;
        *run = candidate;
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

fn build_model_request(agent: &Agent, run: &AgentRun, tools: &[ToolDefinition]) -> ModelRequest {
    let messages = run
        .events()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RunEvent::RunStarted { instruction } => Some(ModelMessage::User {
                content: instruction.clone(),
            }),
            RunEvent::ModelResponseReceived { content } => Some(ModelMessage::Assistant {
                content: content.clone(),
            }),
            RunEvent::ToolCallRequested {
                call_id,
                tool_name,
                arguments,
            } => Some(ModelMessage::ToolCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                arguments: arguments.clone(),
            }),
            RunEvent::ToolCallSucceeded { call_id, output } => Some(ModelMessage::ToolResult {
                call_id: call_id.clone(),
                output: output.clone(),
            }),
            RunEvent::ToolCallFailed { call_id, message } => Some(ModelMessage::ToolError {
                call_id: call_id.clone(),
                message: message.clone(),
            }),
            RunEvent::InputReceived { content } => Some(ModelMessage::User {
                content: content.clone(),
            }),
            RunEvent::ModelRequestStarted
            | RunEvent::InputRequested { .. }
            | RunEvent::RunCompleted { .. }
            | RunEvent::RunFailed { .. }
            | RunEvent::RunCancelled => None,
        })
        .collect();

    ModelRequest {
        instructions: agent.instructions.clone(),
        messages,
        tools: tools.to_vec(),
    }
}

#[derive(Debug)]
pub enum AgentRuntimeError<ModelError> {
    AgentMismatch {
        expected_agent_id: String,
        actual_agent_id: String,
    },
    Model(ModelError),
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
            Self::AgentMismatch {
                expected_agent_id,
                actual_agent_id,
            } => write!(
                formatter,
                "run belongs to agent {actual_agent_id}, not agent {expected_agent_id}"
            ),
            Self::Model(error) => write!(formatter, "model request failed: {error}"),
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
            Self::AgentRun(error) => Some(error),
            Self::History(error) => Some(error),
            Self::EventStore(error) => Some(error),
            Self::AgentMismatch { .. }
            | Self::InterruptedRun { .. }
            | Self::RunOutOfDate { .. }
            | Self::RunHistoryConflict { .. }
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
