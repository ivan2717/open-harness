use std::{error::Error, fmt, time::SystemTime};

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
    pub id: String,
    pub agent_id: String,
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

#[derive(Debug, Clone)]
pub struct RunEventEnvelope {
    pub sequence: u64,
    pub step: u32,
    pub occurred_at: SystemTime,
    pub event: RunEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_run() -> AgentRun {
        AgentRun::new("run-1", "agent-1", "Research Rust ownership")
    }

    #[test]
    fn a_new_run_starts_with_one_event() {
        let run = test_run();

        assert_eq!(run.status(), RunStatus::Running);
        assert_eq!(run.instruction(), "Research Rust ownership");
        assert_eq!(run.events().len(), 1);
        assert_eq!(run.events()[0].sequence, 0);
        assert_eq!(run.events()[0].step, 0);
        assert!(matches!(run.events()[0].event, RunEvent::RunStarted { .. }));
    }

    #[test]
    fn events_are_ordered_and_grouped_by_step() {
        let mut run = test_run();

        assert_eq!(run.begin_step().unwrap(), 1);
        run.record(RunEvent::ModelResponseReceived {
            content: "I need to inspect the project.".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallRequested {
            call_id: "call-1".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments: "{\"path\":\"Cargo.toml\"}".to_owned(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id: "call-1".to_owned(),
            output: "[workspace]".to_owned(),
        })
        .unwrap();

        assert_eq!(run.events().len(), 5);
        assert_eq!(run.events()[4].sequence, 4);
        assert!(run.events()[1..].iter().all(|event| event.step == 1));
        assert_eq!(run.status(), RunStatus::Running);
    }

    #[test]
    fn waiting_for_input_can_resume() {
        let mut run = test_run();
        run.begin_step().unwrap();
        run.record(RunEvent::InputRequested {
            prompt: "Which repository should I inspect?".to_owned(),
        })
        .unwrap();

        assert_eq!(run.status(), RunStatus::WaitingForInput);

        run.record(RunEvent::InputReceived {
            content: "open-agent".to_owned(),
        })
        .unwrap();

        assert_eq!(run.status(), RunStatus::Running);
    }

    #[test]
    fn a_terminal_run_rejects_more_events() {
        let mut run = test_run();
        run.begin_step().unwrap();
        run.record(RunEvent::RunCompleted {
            answer: "Ownership gives each value one owner.".to_owned(),
        })
        .unwrap();

        let error = run.record(RunEvent::RunCancelled).unwrap_err();

        assert_eq!(
            error,
            AgentRunError::RunAlreadyFinished {
                status: RunStatus::Completed,
            }
        );
    }

    #[test]
    fn step_events_require_begin_step() {
        let mut run = test_run();

        let error = run
            .record(RunEvent::ModelResponseReceived {
                content: "response".to_owned(),
            })
            .unwrap_err();

        assert_eq!(
            error,
            AgentRunError::NoActiveStep {
                event: "ModelResponseReceived",
            }
        );
    }

    #[test]
    fn input_received_requires_a_started_step() {
        let mut run = test_run();

        let error = run
            .record(RunEvent::InputReceived {
                content: "open-agent".to_owned(),
            })
            .unwrap_err();

        assert_eq!(
            error,
            AgentRunError::NoActiveStep {
                event: "InputReceived",
            }
        );
    }

    #[test]
    fn failure_and_cancellation_do_not_require_a_started_step() {
        let mut failed_run = test_run();
        failed_run
            .record(RunEvent::RunFailed {
                message: "model configuration is invalid".to_owned(),
            })
            .unwrap();

        let mut cancelled_run = test_run();
        cancelled_run.record(RunEvent::RunCancelled).unwrap();

        assert_eq!(failed_run.status(), RunStatus::Failed);
        assert_eq!(failed_run.events().last().unwrap().step, 0);
        assert_eq!(cancelled_run.status(), RunStatus::Cancelled);
        assert_eq!(cancelled_run.events().last().unwrap().step, 0);
    }
}
