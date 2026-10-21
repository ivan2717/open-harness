use agent_runtime::ContentBlock;
#[path = "support/scripted_model.rs"]
mod scripted_model;
mod support;

use std::{
    convert::Infallible,
    future::{Future, pending},
    task::{Context, Poll, Waker},
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, EventStore, Model, ModelRequest, ModelResponse, ToolCall,
    ToolDefinition, ToolExecutor,
};
use scripted_model::ScriptedModel;
use support::test_context_manager;

struct EchoTools {
    definitions: Vec<ToolDefinition>,
}

impl EchoTools {
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

impl ToolExecutor for EchoTools {
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
            ModelResponse::tool_calls(
                "I will read both files.".to_owned(),
                vec![
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
            ),
            ModelResponse::final_answer("Both files were read.".to_owned()),
        ]),
        EchoTools::new(),
        test_context_manager(),
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
    assert_eq!(requests[1].messages.len(), 4);
    assert_eq!(
        requests[1].messages[1]
            .tool_calls()
            .iter()
            .map(|call| call.id.as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
    assert!(requests[1].messages[2].content.iter().any(|block| matches!(block, ContentBlock::ToolResult { call_id, is_error: false, .. } if call_id == "call-1")));
    assert!(requests[1].messages[3].content.iter().any(|block| matches!(block, ContentBlock::ToolResult { call_id, is_error: false, .. } if call_id == "call-2")));
}

#[tokio::test]
async fn duplicate_tool_call_ids_are_rejected_before_execution() {
    let agent = Agent::new("agent-1", "Agent", "Test agent", "Be precise.");
    let mut run = agent.start_run("duplicate-call-id", "Read two files");
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![ModelResponse::tool_calls(
            "I will read both files.".to_owned(),
            vec![
                ToolCall {
                    id: "reused-id".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: "Cargo.toml".to_owned(),
                },
                ToolCall {
                    id: "reused-id".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: "api/Cargo.toml".to_owned(),
                },
            ],
        )]),
        EchoTools::new(),
        test_context_manager(),
        Some(2),
    );

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AgentRuntimeError::DuplicateToolCallId { ref call_id } if call_id == "reused-id"
    ));
    assert_eq!(run.status(), RunStatus::Failed);
    assert!(
        !run.events()
            .iter()
            .any(|envelope| matches!(&envelope.event, RunEvent::ToolCallRequested { .. }))
    );
}

#[tokio::test]
async fn an_interrupted_execution_requires_explicit_cancellation() {
    let agent = Agent::new("agent-1", "Agent", "Test agent", "Be precise.");
    let mut run = agent.start_run("run-1", "Wait for the model");
    let runtime = AgentRuntime::new(
        PendingModel,
        EchoTools::new(),
        test_context_manager(),
        Some(4),
    );
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
