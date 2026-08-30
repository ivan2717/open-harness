use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt,
    future::Future,
    sync::Mutex,
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, Model, ModelMessage, ModelRequest, ModelResponse, ToolCall,
    ToolDefinition, ToolExecutor,
};

#[derive(Debug)]
struct FakeError(&'static str);

impl fmt::Display for FakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for FakeError {}

struct FakeModel {
    responses: Mutex<VecDeque<Result<ModelResponse, FakeError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl FakeModel {
    fn new(responses: Vec<Result<ModelResponse, FakeError>>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Model for FakeModel {
    type Error = FakeError;

    fn respond(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, Self::Error>> + Send {
        self.requests.lock().unwrap().push(request);

        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(FakeError("no scripted model response")));

        async move { response }
    }
}

struct FakeToolExecutor {
    definitions: Vec<ToolDefinition>,
    failing_tools: HashSet<String>,
    calls: Mutex<Vec<ToolCall>>,
}

impl FakeToolExecutor {
    fn new(failing_tools: &[&str]) -> Self {
        Self {
            definitions: vec![
                ToolDefinition {
                    name: "read_file".to_owned(),
                    description: "Read a file".to_owned(),
                    input_schema: r#"{"type":"object"}"#.to_owned(),
                },
                ToolDefinition {
                    name: "unstable_tool".to_owned(),
                    description: "A tool that can fail".to_owned(),
                    input_schema: r#"{"type":"object"}"#.to_owned(),
                },
            ],
            failing_tools: failing_tools
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<ToolCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl ToolExecutor for FakeToolExecutor {
    type Error = FakeError;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    fn execute(&self, call: ToolCall) -> impl Future<Output = Result<String, Self::Error>> + Send {
        self.calls.lock().unwrap().push(call.clone());

        let should_fail = self.failing_tools.contains(&call.name);
        let output = format!("executed {} with {}", call.name, call.arguments);

        async move {
            if should_fail {
                Err(FakeError("tool execution failed"))
            } else {
                Ok(output)
            }
        }
    }
}

fn test_agent() -> Agent {
    Agent::new(
        "agent-1",
        "Research agent",
        "Researches a question with tools",
        "Be precise and cite the tool results.",
    )
}

fn test_run(agent: &Agent) -> AgentRun {
    agent.start_run("run-1", "Read Cargo.toml and explain the workspace")
}

fn event_names(run: &AgentRun) -> Vec<&'static str> {
    run.events()
        .iter()
        .map(|envelope| match &envelope.event {
            RunEvent::RunStarted { .. } => "RunStarted",
            RunEvent::ModelRequestStarted => "ModelRequestStarted",
            RunEvent::ModelResponseReceived { .. } => "ModelResponseReceived",
            RunEvent::ToolCallRequested { .. } => "ToolCallRequested",
            RunEvent::ToolCallSucceeded { .. } => "ToolCallSucceeded",
            RunEvent::ToolCallFailed { .. } => "ToolCallFailed",
            RunEvent::InputRequested { .. } => "InputRequested",
            RunEvent::InputReceived { .. } => "InputReceived",
            RunEvent::RunCompleted { .. } => "RunCompleted",
            RunEvent::RunFailed { .. } => "RunFailed",
            RunEvent::RunCancelled => "RunCancelled",
        })
        .collect()
}

#[tokio::test]
async fn a_final_answer_completes_the_run() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![Ok(ModelResponse::FinalAnswer {
            answer: "This is a Cargo workspace.".to_owned(),
        })]),
        FakeToolExecutor::new(&[]),
        None,
    );

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert_eq!(run.current_step(), 1);
    assert_eq!(
        event_names(&run),
        vec![
            "RunStarted",
            "ModelRequestStarted",
            "ModelResponseReceived",
            "RunCompleted",
        ]
    );
}

#[tokio::test]
async fn tool_results_are_added_to_the_next_model_request() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![
            Ok(ModelResponse::ToolCalls {
                content: "I will inspect the manifest.".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: r#"{"path":"Cargo.toml"}"#.to_owned(),
                }],
            }),
            Ok(ModelResponse::FinalAnswer {
                answer: "The manifest defines a workspace.".to_owned(),
            }),
        ]),
        FakeToolExecutor::new(&[]),
        Some(4),
    );

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert_eq!(run.current_step(), 2);
    assert_eq!(runtime.tools().calls().len(), 1);
    assert_eq!(
        event_names(&run),
        vec![
            "RunStarted",
            "ModelRequestStarted",
            "ModelResponseReceived",
            "ToolCallRequested",
            "ToolCallSucceeded",
            "ModelRequestStarted",
            "ModelResponseReceived",
            "RunCompleted",
        ]
    );

    let requests = runtime.model().requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].instructions, agent.instructions);
    assert!(requests[1].messages.iter().any(|message| matches!(
        message,
        ModelMessage::ToolResult { call_id, output }
            if call_id == "call-1" && output.contains("read_file")
    )));
}

#[tokio::test]
async fn a_tool_failure_is_visible_to_the_model_and_can_be_recovered() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![
            Ok(ModelResponse::ToolCalls {
                content: "I will try the tool.".to_owned(),
                calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "unstable_tool".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            }),
            Ok(ModelResponse::FinalAnswer {
                answer: "The tool failed, so I stopped using it.".to_owned(),
            }),
        ]),
        FakeToolExecutor::new(&["unstable_tool"]),
        Some(4),
    );

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert!(event_names(&run).contains(&"ToolCallFailed"));

    let requests = runtime.model().requests();
    assert!(requests[1].messages.iter().any(|message| matches!(
        message,
        ModelMessage::ToolError { call_id, message }
            if call_id == "call-1" && message == "tool execution failed"
    )));
}

#[tokio::test]
async fn a_run_can_wait_for_input_and_resume() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![
            Ok(ModelResponse::InputRequired {
                prompt: "Which file should I read?".to_owned(),
            }),
            Ok(ModelResponse::FinalAnswer {
                answer: "I will read Cargo.toml.".to_owned(),
            }),
        ]),
        FakeToolExecutor::new(&[]),
        Some(4),
    );

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(status, RunStatus::WaitingForInput);

    run.record(RunEvent::InputReceived {
        content: "Cargo.toml".to_owned(),
    })
    .unwrap();

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert_eq!(run.current_step(), 2);

    let requests = runtime.model().requests();
    assert!(requests[1].messages.iter().any(|message| matches!(
        message,
        ModelMessage::User { content } if content == "Cargo.toml"
    )));
}

#[tokio::test]
async fn a_model_failure_fails_the_run() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![Err(FakeError("model unavailable"))]),
        FakeToolExecutor::new(&[]),
        Some(4),
    );

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(error, AgentRuntimeError::Model(_)));
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(event_names(&run).last(), Some(&"RunFailed"));
}

#[tokio::test]
async fn the_step_limit_stops_an_infinite_tool_loop() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![Ok(ModelResponse::ToolCalls {
            content: "I need another tool call.".to_owned(),
            calls: vec![ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"Cargo.toml"}"#.to_owned(),
            }],
        })]),
        FakeToolExecutor::new(&[]),
        Some(1),
    );

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AgentRuntimeError::StepLimitExceeded { max_steps: 1 }
    ));
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(event_names(&run).last(), Some(&"RunFailed"));
}

#[tokio::test]
async fn an_empty_tool_call_response_fails_the_run() {
    let agent = test_agent();
    let mut run = test_run(&agent);
    let runtime = AgentRuntime::new(
        FakeModel::new(vec![Ok(ModelResponse::ToolCalls {
            content: "I will call a tool.".to_owned(),
            calls: Vec::new(),
        })]),
        FakeToolExecutor::new(&[]),
        Some(4),
    );

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(error, AgentRuntimeError::EmptyToolCalls));
    assert_eq!(run.status(), RunStatus::Failed);
}

#[tokio::test]
async fn a_runtime_rejects_a_run_for_another_agent() {
    let agent = test_agent();
    let mut run = AgentRun::new("run-1", "another-agent", "Do something");
    let runtime = AgentRuntime::new(
        FakeModel::new(Vec::new()),
        FakeToolExecutor::new(&[]),
        Some(4),
    );

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(error, AgentRuntimeError::AgentMismatch { .. }));
    assert_eq!(run.current_step(), 0);
    assert_eq!(event_names(&run), vec!["RunStarted"]);
}
