use std::{
    collections::VecDeque,
    convert::Infallible,
    future::{Future, pending},
    sync::Mutex,
    task::{Context, Poll, Waker},
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, EventStore, Model, ModelMessage, ModelRequest, ModelResponse,
    ToolCall, ToolDefinition, ToolExecutor,
};

struct ScriptedModel {
    responses: Mutex<VecDeque<ModelResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Model for ScriptedModel {
    type Error = Infallible;

    fn respond(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, Self::Error>> + Send {
        self.requests.lock().unwrap().push(request);
        let response = self.responses.lock().unwrap().pop_front().unwrap();

        async move { Ok(response) }
    }
}

struct RecordingTools {
    definitions: Vec<ToolDefinition>,
}

impl RecordingTools {
    fn new() -> Self {
        Self {
            definitions: vec![ToolDefinition {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: r#"{"type":"object"}"#.to_owned(),
            }],
        }
    }
}

impl ToolExecutor for RecordingTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn execute(&self, call: ToolCall) -> Result<String, Self::Error> {
        Ok(format!("contents of {}", call.arguments))
    }
}

struct PendingModel;

impl Model for PendingModel {
    type Error = Infallible;

    fn respond(
        &self,
        _request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, Self::Error>> + Send {
        pending()
    }
}

#[tokio::test]
async fn all_tool_calls_are_declared_before_any_result() {
    let agent = Agent::new("agent-1", "Agent", "Test agent", "Be precise.");
    let mut run = agent.start_run("run-1", "Read two files");
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::ToolCalls {
                content: "I will read both files.".to_owned(),
                calls: vec![
                    ToolCall {
                        id: "call-1".to_owned(),
                        name: "read_file".to_owned(),
                        arguments: "Cargo.toml".to_owned(),
                    },
                    ToolCall {
                        id: "call-2".to_owned(),
                        name: "read_file".to_owned(),
                        arguments: "api/Cargo.toml".to_owned(),
                    },
                ],
            },
            ModelResponse::FinalAnswer {
                answer: "Both files were read.".to_owned(),
            },
        ]),
        RecordingTools::new(),
        Some(4),
    );

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert!(matches!(
        &run.events()[3].event,
        RunEvent::ToolCallRequested { call_id, .. } if call_id == "call-1"
    ));
    assert!(matches!(
        &run.events()[4].event,
        RunEvent::ToolCallRequested { call_id, .. } if call_id == "call-2"
    ));
    assert!(matches!(
        &run.events()[5].event,
        RunEvent::ToolCallSucceeded { call_id, .. } if call_id == "call-1"
    ));
    assert!(matches!(
        &run.events()[6].event,
        RunEvent::ToolCallSucceeded { call_id, .. } if call_id == "call-2"
    ));

    let requests = runtime.model().requests();
    assert!(matches!(
        &requests[1].messages[2],
        ModelMessage::ToolCall { call_id, .. } if call_id == "call-1"
    ));
    assert!(matches!(
        &requests[1].messages[3],
        ModelMessage::ToolCall { call_id, .. } if call_id == "call-2"
    ));
    assert!(matches!(
        &requests[1].messages[4],
        ModelMessage::ToolResult { call_id, .. } if call_id == "call-1"
    ));
    assert!(matches!(
        &requests[1].messages[5],
        ModelMessage::ToolResult { call_id, .. } if call_id == "call-2"
    ));
}

#[tokio::test]
async fn an_interrupted_execution_requires_explicit_cancellation() {
    let agent = Agent::new("agent-1", "Agent", "Test agent", "Be precise.");
    let mut run = agent.start_run("run-1", "Wait for the model");
    let runtime = AgentRuntime::new(PendingModel, RecordingTools::new(), Some(4));
    let mut notifications = runtime.subscribe();

    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut context = Context::from_waker(Waker::noop());

    assert!(matches!(
        execution.as_mut().poll(&mut context),
        Poll::Pending
    ));

    drop(execution);

    assert_eq!(run.status(), RunStatus::Running);
    assert!(matches!(
        &run.events().last().unwrap().event,
        RunEvent::ModelRequestStarted
    ));

    let stored = runtime.event_store().load("run-1").await.unwrap().unwrap();
    assert!(matches!(
        &stored.events.last().unwrap().event,
        RunEvent::ModelRequestStarted
    ));

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentRuntimeError::InterruptedRun {
            last_sequence: 1,
            ..
        }
    ));

    runtime.cancel_run(&mut run).await.unwrap();

    assert_eq!(run.status(), RunStatus::Cancelled);
    let stored = runtime.event_store().load("run-1").await.unwrap().unwrap();
    assert!(matches!(
        &stored.events.last().unwrap().event,
        RunEvent::RunCancelled
    ));
    assert_eq!(notifications.try_recv().unwrap().envelope.sequence, 0);
    assert_eq!(notifications.try_recv().unwrap().envelope.sequence, 1);
    assert_eq!(notifications.try_recv().unwrap().envelope.sequence, 2);
}
