mod support;

use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ContextInputRef, ContextManager, ContextMeasurement, Model,
    ModelOutput, ModelRequest, ModelResponse, PreparedContext, ToolCall, ToolDefinition,
    ToolExecutor,
};
use support::test_context_manager;

type Catalog = Arc<Mutex<Vec<ToolDefinition>>>;

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: name.into(),
        input_schema: r#"{"type":"object","properties":{}}"#.into(),
    }
}

struct DynamicTools {
    catalog: Catalog,
    snapshots: AtomicUsize,
    calls: Mutex<Vec<String>>,
}

impl ToolExecutor for DynamicTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &[] // Only snapshot_definitions represents the current configuration.
    }

    fn snapshot_definitions(&self) -> Vec<ToolDefinition> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        self.catalog.lock().unwrap().clone()
    }

    async fn execute(&self, call: ToolCall) -> Result<String, Self::Error> {
        self.calls.lock().unwrap().push(call.name);
        Ok("done".into())
    }
}

struct TrackingContext<C> {
    inner: C,
    catalogs: Mutex<Vec<(&'static str, Vec<ToolDefinition>)>>,
}

impl<C: ContextManager> ContextManager for TrackingContext<C> {
    type Error = C::Error;

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        self.inner.prepare(input).await
    }

    fn measure_request(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Option<ContextMeasurement> {
        self.catalogs
            .lock()
            .unwrap()
            .push(("measure", input.tools.to_vec()));
        self.inner.measure_request(input, request)
    }

    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        self.catalogs
            .lock()
            .unwrap()
            .push(("observe", input.tools.to_vec()));
        self.inner.observe_response(input, output).await
    }

    async fn compact_now(
        &self,
        input: ContextInputRef<'_>,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        self.catalogs
            .lock()
            .unwrap()
            .push(("compact", input.tools.to_vec()));
        Ok(None)
    }
}

struct ChangingModel {
    catalog: Catalog,
    responses: Mutex<VecDeque<ModelResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl Model for ChangingModel {
    type Error = Infallible;

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.requests.lock().unwrap().push(request);
        // Simulate an extension being enabled while the model is responding.
        *self.catalog.lock().unwrap() = vec![definition("read_file"), definition("extension")];
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted response"))
    }
}

fn call(name: &str) -> ModelResponse {
    ModelResponse::tool_calls(
        String::new(),
        vec![ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: "{}".into(),
        }],
    )
}

#[tokio::test]
async fn tools_are_stable_within_a_turn_and_refreshed_for_follow_up_input() {
    let catalog = Arc::new(Mutex::new(vec![definition("read_file")]));
    let runtime = AgentRuntime::new(
        ChangingModel {
            catalog: catalog.clone(),
            responses: Mutex::new(VecDeque::from([
                call("read_file"),
                ModelResponse::final_answer("done".into()),
                ModelResponse::final_answer("next".into()),
            ])),
            requests: Mutex::default(),
        },
        DynamicTools {
            catalog,
            snapshots: AtomicUsize::new(0),
            calls: Mutex::default(),
        },
        TrackingContext {
            inner: test_context_manager(),
            catalogs: Mutex::default(),
        },
        Some(5),
    )
    .with_continue_after_response(true);
    let agent = Agent::new("agent", "Agent", "Test", "Be helpful");
    let mut run = agent.start_run("run", "Read a file");

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    assert_eq!(runtime.tools().snapshots.load(Ordering::SeqCst), 1);
    assert_eq!(*runtime.tools().calls.lock().unwrap(), ["read_file"]);
    runtime
        .submit_input(&mut run, "Use the extension")
        .await
        .unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(runtime.tools().snapshots.load(Ordering::SeqCst), 2);
    assert!(!runtime.compact_now(&agent, &mut run).await.unwrap());
    assert_eq!(runtime.tools().snapshots.load(Ordering::SeqCst), 3);
    let catalogs = runtime.context_manager().catalogs.lock().unwrap();
    let initial = vec![definition("read_file")];
    let refreshed = vec![definition("read_file"), definition("extension")];
    assert_eq!(
        *catalogs,
        vec![
            ("measure", initial.clone()),
            ("observe", initial.clone()),
            ("measure", initial.clone()),
            ("observe", initial),
            ("measure", refreshed.clone()),
            ("observe", refreshed.clone()),
            ("compact", refreshed),
        ]
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].tools, vec![definition("read_file")]);
    assert_eq!(requests[1].tools, requests[0].tools);
    assert_eq!(
        requests[2].tools,
        vec![definition("read_file"), definition("extension")]
    );
}

#[tokio::test]
async fn a_tool_enabled_mid_turn_cannot_be_called_until_the_next_turn() {
    let catalog = Arc::new(Mutex::new(vec![definition("read_file")]));
    let runtime = AgentRuntime::new(
        ChangingModel {
            catalog: catalog.clone(),
            responses: Mutex::new(VecDeque::from([
                call("extension"),
                ModelResponse::final_answer("done".into()),
            ])),
            requests: Mutex::default(),
        },
        DynamicTools {
            catalog,
            snapshots: AtomicUsize::new(0),
            calls: Mutex::default(),
        },
        test_context_manager(),
        Some(5),
    );
    let agent = Agent::new("agent", "Agent", "Test", "Be helpful");
    let mut run = agent.start_run("run", "Use tools");
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert!(runtime.tools().calls.lock().unwrap().is_empty());
    assert!(run.events().iter().any(|event| matches!(
        &event.event,
        RunEvent::ToolCallFailed { message, .. } if message.contains("not available in this turn")
    )));
}

#[tokio::test]
async fn refreshed_definitions_are_validated_before_contacting_the_model() {
    let catalog = Arc::new(Mutex::new(vec![
        definition("duplicate"),
        definition("duplicate"),
    ]));
    let runtime = AgentRuntime::new(
        ChangingModel {
            catalog: catalog.clone(),
            responses: Mutex::default(),
            requests: Mutex::default(),
        },
        DynamicTools {
            catalog,
            snapshots: AtomicUsize::new(0),
            calls: Mutex::default(),
        },
        test_context_manager(),
        Some(5),
    );
    let agent = Agent::new("agent", "Agent", "Test", "Be helpful");
    let mut run = agent.start_run("run", "Use tools");
    assert!(matches!(runtime.run_until_blocked(&agent, &mut run).await,
        Err(AgentRuntimeError::DuplicateToolName { name }) if name == "duplicate"));
    assert!(runtime.model().requests.lock().unwrap().is_empty());
}
