use std::{error::Error, fmt, time::SystemTime};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
}

impl Agent {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            instructions: instructions.into(),
        }
    }

    pub fn start_run(&self, run_id: impl Into<String>, instruction: impl Into<String>) -> AgentRun {
        AgentRun::new(run_id, self.id.clone(), instruction)
    }
}

#[derive(Debug, Clone)]
pub struct AgentRun {
    id: String,
    agent_id: String,
    status: RunStatus,
    current_step: u32,
    events: Vec<RunEventEnvelope>,
}

impl AgentRun {
    pub fn new(
        id: impl Into<String>,
        agent_id: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        let mut run = Self {
            id: id.into(),
            agent_id: agent_id.into(),
            status: RunStatus::Running,
            current_step: 0,
            events: Vec::new(),
        };

        run.append_event(RunEvent::RunStarted {
            instruction: instruction.into(),
        });

        run
    }

    pub fn restore(
        id: impl Into<String>,
        agent_id: impl Into<String>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<Self, AgentRunHistoryError> {
        let Some(first) = events.first() else {
            return Err(AgentRunHistoryError::EmptyHistory);
        };

        if first.sequence != 0
            || first.step != 0
            || !matches!(first.event, RunEvent::RunStarted { .. })
        {
            return Err(AgentRunHistoryError::InvalidFirstEvent {
                sequence: first.sequence,
                step: first.step,
                event: first.event.name(),
            });
        }

        let mut run = Self {
            id: id.into(),
            agent_id: agent_id.into(),
            status: RunStatus::Running,
            current_step: 0,
            events: vec![first.clone()],
        };

        for (index, envelope) in events.into_iter().enumerate().skip(1) {
            let expected_sequence = index as u64;
            if envelope.sequence != expected_sequence {
                return Err(AgentRunHistoryError::UnexpectedSequence {
                    index,
                    expected: expected_sequence,
                    actual: envelope.sequence,
                });
            }

            match &envelope.event {
                RunEvent::RunStarted { .. } => {
                    return Err(AgentRunHistoryError::DuplicateRunStarted {
                        sequence: envelope.sequence,
                    });
                }
                RunEvent::ModelRequestStarted => {
                    if !run.is_running() {
                        return Err(AgentRunHistoryError::InvalidEvent {
                            sequence: envelope.sequence,
                            source: AgentRunError::RunNotRunning { status: run.status },
                        });
                    }

                    let expected_step = run.current_step.checked_add(1).ok_or(
                        AgentRunHistoryError::InvalidEvent {
                            sequence: envelope.sequence,
                            source: AgentRunError::StepNumberOverflow,
                        },
                    )?;
                    run.validate_restored_step(&envelope, expected_step)?;
                    run.current_step = expected_step;
                }
                event => {
                    run.validate_restored_step(&envelope, run.current_step)?;

                    if event.requires_active_step() && run.current_step == 0 {
                        return Err(AgentRunHistoryError::InvalidEvent {
                            sequence: envelope.sequence,
                            source: AgentRunError::NoActiveStep {
                                event: event.name(),
                            },
                        });
                    }

                    run.status = run.next_status(event).map_err(|source| {
                        AgentRunHistoryError::InvalidEvent {
                            sequence: envelope.sequence,
                            source,
                        }
                    })?;
                }
            }

            run.events.push(envelope);
        }

        Ok(run)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn status(&self) -> RunStatus {
        self.status
    }

    pub fn is_running(&self) -> bool {
        self.status == RunStatus::Running
    }

    pub fn current_step(&self) -> u32 {
        self.current_step
    }

    pub fn events(&self) -> &[RunEventEnvelope] {
        &self.events
    }

    pub fn instruction(&self) -> &str {
        match &self.events[0].event {
            RunEvent::RunStarted { instruction } => instruction,
            _ => unreachable!("the first run event is always RunStarted"),
        }
    }

    pub fn begin_step(&mut self) -> Result<u32, AgentRunError> {
        if !self.is_running() {
            return Err(AgentRunError::RunNotRunning {
                status: self.status,
            });
        }

        self.current_step = self
            .current_step
            .checked_add(1)
            .ok_or(AgentRunError::StepNumberOverflow)?;

        self.append_event(RunEvent::ModelRequestStarted);

        Ok(self.current_step)
    }

    pub fn record(&mut self, event: RunEvent) -> Result<&RunEventEnvelope, AgentRunError> {
        if matches!(
            event,
            RunEvent::RunStarted { .. } | RunEvent::ModelRequestStarted
        ) {
            return Err(AgentRunError::ReservedEvent {
                event: event.name(),
            });
        }

        if event.requires_active_step() && self.current_step == 0 {
            return Err(AgentRunError::NoActiveStep {
                event: event.name(),
            });
        }

        let next_status = self.next_status(&event)?;
        self.status = next_status;
        self.append_event(event);

        Ok(self
            .events
            .last()
            .expect("the event was appended immediately before this access"))
    }

    fn validate_restored_step(
        &self,
        envelope: &RunEventEnvelope,
        expected: u32,
    ) -> Result<(), AgentRunHistoryError> {
        if envelope.step != expected {
            return Err(AgentRunHistoryError::UnexpectedStep {
                sequence: envelope.sequence,
                event: envelope.event.name(),
                expected,
                actual: envelope.step,
            });
        }

        Ok(())
    }

    fn next_status(&self, event: &RunEvent) -> Result<RunStatus, AgentRunError> {
        if self.status.is_terminal() {
            return Err(AgentRunError::RunAlreadyFinished {
                status: self.status,
            });
        }

        match (self.status, event) {
            (RunStatus::Running, RunEvent::ModelResponseReceived { .. })
            | (RunStatus::Running, RunEvent::ToolCallRequested { .. })
            | (RunStatus::Running, RunEvent::ToolCallSucceeded { .. })
            | (RunStatus::Running, RunEvent::ToolCallFailed { .. }) => Ok(RunStatus::Running),

            (RunStatus::Running, RunEvent::InputRequested { .. }) => Ok(RunStatus::WaitingForInput),
            (RunStatus::WaitingForInput, RunEvent::InputReceived { .. }) => Ok(RunStatus::Running),
            (RunStatus::Running, RunEvent::RunCompleted { .. }) => Ok(RunStatus::Completed),
            (RunStatus::Running | RunStatus::WaitingForInput, RunEvent::RunFailed { .. }) => {
                Ok(RunStatus::Failed)
            }
            (RunStatus::Running | RunStatus::WaitingForInput, RunEvent::RunCancelled) => {
                Ok(RunStatus::Cancelled)
            }

            (_, RunEvent::RunStarted { .. } | RunEvent::ModelRequestStarted) => {
                Err(AgentRunError::ReservedEvent {
                    event: event.name(),
                })
            }
            _ => Err(AgentRunError::InvalidTransition {
                status: self.status,
                event: event.name(),
            }),
        }
    }

    fn append_event(&mut self, event: RunEvent) {
        self.events.push(RunEventEnvelope {
            sequence: self.events.len() as u64,
            step: self.current_step,
            occurred_at: SystemTime::now(),
            event,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    pub sequence: u64,
    pub step: u32,
    pub occurred_at: SystemTime,
    pub event: RunEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    RunStarted {
        instruction: String,
    },
    ModelRequestStarted,
    ModelResponseReceived {
        content: String,
    },
    ToolCallRequested {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallSucceeded {
        call_id: String,
        output: String,
    },
    ToolCallFailed {
        call_id: String,
        message: String,
    },
    InputRequested {
        prompt: String,
    },
    InputReceived {
        content: String,
    },
    RunCompleted {
        answer: String,
    },
    RunFailed {
        message: String,
    },
    RunCancelled,
}

impl RunEvent {
    fn name(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "RunStarted",
            Self::ModelRequestStarted => "ModelRequestStarted",
            Self::ModelResponseReceived { .. } => "ModelResponseReceived",
            Self::ToolCallRequested { .. } => "ToolCallRequested",
            Self::ToolCallSucceeded { .. } => "ToolCallSucceeded",
            Self::ToolCallFailed { .. } => "ToolCallFailed",
            Self::InputRequested { .. } => "InputRequested",
            Self::InputReceived { .. } => "InputReceived",
            Self::RunCompleted { .. } => "RunCompleted",
            Self::RunFailed { .. } => "RunFailed",
            Self::RunCancelled => "RunCancelled",
        }
    }

    fn requires_active_step(&self) -> bool {
        match self {
            Self::RunStarted { .. }
            | Self::ModelRequestStarted
            | Self::RunFailed { .. }
            | Self::RunCancelled => false,

            Self::ModelResponseReceived { .. }
            | Self::ToolCallRequested { .. }
            | Self::ToolCallSucceeded { .. }
            | Self::ToolCallFailed { .. }
            | Self::InputRequested { .. }
            | Self::InputReceived { .. }
            | Self::RunCompleted { .. } => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunHistoryError {
    EmptyHistory,
    InvalidFirstEvent {
        sequence: u64,
        step: u32,
        event: &'static str,
    },
    UnexpectedSequence {
        index: usize,
        expected: u64,
        actual: u64,
    },
    UnexpectedStep {
        sequence: u64,
        event: &'static str,
        expected: u32,
        actual: u32,
    },
    DuplicateRunStarted {
        sequence: u64,
    },
    InvalidEvent {
        sequence: u64,
        source: AgentRunError,
    },
}

impl fmt::Display for AgentRunHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHistory => formatter.write_str("cannot restore a run from empty history"),
            Self::InvalidFirstEvent {
                sequence,
                step,
                event,
            } => write!(
                formatter,
                "the first event must be RunStarted at sequence 0 and step 0, got {event} at sequence {sequence} and step {step}"
            ),
            Self::UnexpectedSequence {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "event at index {index} has sequence {actual}, expected {expected}"
            ),
            Self::UnexpectedStep {
                sequence,
                event,
                expected,
                actual,
            } => write!(
                formatter,
                "event {event} at sequence {sequence} belongs to step {actual}, expected {expected}"
            ),
            Self::DuplicateRunStarted { sequence } => write!(
                formatter,
                "RunStarted may only be the first event, but another occurred at sequence {sequence}"
            ),
            Self::InvalidEvent { sequence, source } => {
                write!(
                    formatter,
                    "event at sequence {sequence} is invalid: {source}"
                )
            }
        }
    }
}

impl Error for AgentRunHistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidEvent { source, .. } => Some(source),
            Self::EmptyHistory
            | Self::InvalidFirstEvent { .. }
            | Self::UnexpectedSequence { .. }
            | Self::UnexpectedStep { .. }
            | Self::DuplicateRunStarted { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunError {
    RunNotRunning {
        status: RunStatus,
    },
    RunAlreadyFinished {
        status: RunStatus,
    },
    NoActiveStep {
        event: &'static str,
    },
    InvalidTransition {
        status: RunStatus,
        event: &'static str,
    },
    ReservedEvent {
        event: &'static str,
    },
    StepNumberOverflow,
}

impl fmt::Display for AgentRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunNotRunning { status } => {
                write!(formatter, "cannot begin a step while the run is {status:?}")
            }
            Self::RunAlreadyFinished { status } => {
                write!(
                    formatter,
                    "cannot record an event after the run became {status:?}"
                )
            }
            Self::NoActiveStep { event } => {
                write!(formatter, "event {event} requires an active step")
            }
            Self::InvalidTransition { status, event } => {
                write!(
                    formatter,
                    "event {event} is invalid while the run is {status:?}"
                )
            }
            Self::ReservedEvent { event } => {
                write!(formatter, "event {event} must be created by AgentRun")
            }
            Self::StepNumberOverflow => formatter.write_str("the step number overflowed"),
        }
    }
}

impl Error for AgentRunError {}
