use std::{
    error::Error,
    fmt,
    ops::{Deref, DerefMut},
};

use agent_core::agent::{Agent, AgentRun, AgentRunError, RunEvent, RunStatus};

use crate::{Model, ModelMessage, ModelRequest, ModelResponse, ToolDefinition, ToolExecutor};

#[derive(Debug)]
pub struct AgentRuntime<M, T> {
    model: M,
    tools: T,
    max_steps: u32,
}

impl<M, T> AgentRuntime<M, T> {
    pub fn new(model: M, tools: T, max_steps: Option<u32>) -> Self {
        Self {
            model,
            tools,
            max_steps: max_steps.unwrap_or(i32::MAX as u32),
        }
    }

    pub fn model(&self) -> &M {
        &self.model
    }

    pub fn tools(&self) -> &T {
        &self.tools
    }

    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }
}

impl<M, T> AgentRuntime<M, T>
where
    M: Model,
    T: ToolExecutor,
{
    pub async fn run_until_blocked(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
    ) -> Result<RunStatus, AgentRuntimeError<M::Error>> {
        if run.agent_id != agent.id {
            return Err(AgentRuntimeError::AgentMismatch {
                expected_agent_id: agent.id.clone(),
                actual_agent_id: run.agent_id.clone(),
            });
        }

        let mut run = CancelRunOnDrop::new(run);

        while run.is_running() {
            if run.current_step() >= self.max_steps {
                run.record(RunEvent::RunFailed {
                    message: format!("maximum number of steps ({}) reached", self.max_steps),
                })?;

                return Err(AgentRuntimeError::StepLimitExceeded {
                    max_steps: self.max_steps,
                });
            }

            run.begin_step()?;

            let request = build_model_request(agent, &run, self.tools.definitions());
            let response = match self.model.respond(request).await {
                Ok(response) => response,
                Err(source) => {
                    run.record(RunEvent::RunFailed {
                        message: source.to_string(),
                    })?;

                    return Err(AgentRuntimeError::Model(source));
                }
            };

            match response {
                ModelResponse::FinalAnswer { answer } => {
                    run.record(RunEvent::ModelResponseReceived {
                        content: answer.clone(),
                    })?;
                    run.record(RunEvent::RunCompleted { answer })?;
                }
                ModelResponse::ToolCalls { content, calls } => {
                    run.record(RunEvent::ModelResponseReceived { content })?;

                    if calls.is_empty() {
                        run.record(RunEvent::RunFailed {
                            message: "model returned an empty tool-call list".to_owned(),
                        })?;

                        return Err(AgentRuntimeError::EmptyToolCalls);
                    }

                    for call in &calls {
                        run.record(RunEvent::ToolCallRequested {
                            call_id: call.id.clone(),
                            tool_name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        })?;
                    }

                    for call in calls {
                        let call_id = call.id.clone();

                        match self.tools.execute(call).await {
                            Ok(output) => {
                                run.record(RunEvent::ToolCallSucceeded { call_id, output })?;
                            }
                            Err(error) => {
                                run.record(RunEvent::ToolCallFailed {
                                    call_id,
                                    message: error.to_string(),
                                })?;
                            }
                        }
                    }
                }
                ModelResponse::InputRequired { prompt } => {
                    run.record(RunEvent::ModelResponseReceived {
                        content: prompt.clone(),
                    })?;
                    run.record(RunEvent::InputRequested { prompt })?;
                }
            }
        }

        Ok(run.status())
    }
}

struct CancelRunOnDrop<'a> {
    run: &'a mut AgentRun,
}

impl<'a> CancelRunOnDrop<'a> {
    fn new(run: &'a mut AgentRun) -> Self {
        Self { run }
    }
}

impl Deref for CancelRunOnDrop<'_> {
    type Target = AgentRun;

    fn deref(&self) -> &Self::Target {
        self.run
    }
}

impl DerefMut for CancelRunOnDrop<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.run
    }
}

impl Drop for CancelRunOnDrop<'_> {
    fn drop(&mut self) {
        if self.run.is_running() {
            let _ = self.run.record(RunEvent::RunCancelled);
        }
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
            Self::AgentMismatch { .. } | Self::EmptyToolCalls | Self::StepLimitExceeded { .. } => {
                None
            }
        }
    }
}

impl<ModelError> From<AgentRunError> for AgentRuntimeError<ModelError> {
    fn from(error: AgentRunError) -> Self {
        Self::AgentRun(error)
    }
}
