use std::{
    collections::VecDeque,
    convert::Infallible,
    future::{Future, pending},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use agent_core::agent::{Agent, RunEvent, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ApproximateTokenCounter, ContextInputRef, ContextManager,
    ContextPolicy, EventStore, InMemoryEventStore, InMemoryNativeContextCheckpointStore, Model,
    ModelMessage, ModelOutput, ModelRequest, ModelResponse, NativeContextCompactor,
    NativeContextManager, PreparedContext, ProviderContextIdentity, ProviderContextWindow,
    ToolCall, ToolDefinition, ToolExecutor,
};

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "test-provider".to_owned(),
        model: "test-model".to_owned(),
        format: "items-v1".to_owned(),
    }
}

fn tool_response() -> ModelResponse {
    ModelResponse::tool_calls(
        "Inspect both records.".to_owned(),
        ["first", "second"]
            .map(|id| ToolCall {
                id: id.to_owned(),
                name: "inspect".to_owned(),
                arguments: "{}".to_owned(),
            })
            .into(),
    )
}

fn response_window() -> ProviderContextWindow {
    ProviderContextWindow {
        prefix_fingerprint: None,
        identity: identity(),
        items: vec![
            serde_json::json!({"type": "user", "content": "Inspect the records"}),
            serde_json::json!({"type": "reasoning", "encrypted_content": "opaque-state"}),
            serde_json::json!({"type": "function_call", "call_id": "first"}),
            serde_json::json!({"type": "function_call", "call_id": "second"}),
        ],
        input_tokens: 100,
    }
}

struct MetadataModel {
    identity: Option<ProviderContextIdentity>,
    responses: Mutex<VecDeque<ModelOutput>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl MetadataModel {
    fn new(
        identity: Option<ProviderContextIdentity>,
        responses: impl IntoIterator<Item = ModelOutput>,
    ) -> Self {
        Self {
            identity,
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Model for MetadataModel {
    type Error = Infallible;

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        self.identity.as_ref()
    }

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("runtime must call the metadata-aware entry point")
    }

    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self.responses.lock().unwrap().pop_front().unwrap())
    }
}

struct UnusedCompactor(ProviderContextIdentity);

impl NativeContextCompactor for UnusedCompactor {
    type Error = Infallible;

    fn identity(&self) -> &ProviderContextIdentity {
        &self.0
    }

    fn input_token_limit(&self) -> usize {
        100_000
    }

    async fn compact(
        &self,
        _: ModelRequest,
        _: usize,
    ) -> Result<ProviderContextWindow, Self::Error> {
        panic!("these short runs should never need compaction")
    }
}

type NativeManager = NativeContextManager<
    UnusedCompactor,
    InMemoryNativeContextCheckpointStore,
    ApproximateTokenCounter,
>;

struct Observation {
    events: Vec<RunEventEnvelope>,
    effects: usize,
}

struct ObservingContext {
    inner: NativeManager,
    store: InMemoryEventStore,
    effects: Arc<AtomicUsize>,
    observations: Mutex<Vec<Observation>>,
    pause_observation: bool,
    observation_dropped: Arc<AtomicBool>,
}

impl ObservingContext {
    fn new(store: InMemoryEventStore, effects: Arc<AtomicUsize>) -> Self {
        Self {
            inner: NativeContextManager::new(
                ContextPolicy::default(),
                UnusedCompactor(identity()),
                InMemoryNativeContextCheckpointStore::new(),
                ApproximateTokenCounter::default(),
            ),
            store,
            effects,
            observations: Mutex::new(Vec::new()),
            pause_observation: false,
            observation_dropped: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ContextManager for ObservingContext {
    type Error = <NativeManager as ContextManager>::Error;

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        self.inner.prepare(input).await
    }

    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        let stored = self.store.load(input.run_id).await.unwrap().unwrap();
        assert_eq!(
            stored.events, input.events,
            "observers must never checkpoint events that have not been persisted"
        );
        self.observations.lock().unwrap().push(Observation {
            events: input.events.to_vec(),
            effects: self.effects.load(Ordering::SeqCst),
        });
        if self.pause_observation {
            let _cleanup = DropFlag(Arc::clone(&self.observation_dropped));
            pending::<()>().await;
        }
        self.inner.observe_response(input, output).await
    }
}

struct RecordingTools {
    effects: Arc<AtomicUsize>,
    definitions: Vec<ToolDefinition>,
}

impl RecordingTools {
    fn new(effects: Arc<AtomicUsize>) -> Self {
        Self {
            effects,
            definitions: vec![ToolDefinition {
                name: "inspect".to_owned(),
                description: "Inspect a record".to_owned(),
                input_schema: "{}".to_owned(),
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
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(format!("result-{}", call.id))
    }
}

#[tokio::test]
async fn native_response_state_is_observed_before_effects_and_followed_by_each_tool_result_once() {
    let agent = Agent::new("agent", "Agent", "", "Inspect the supplied records.");
    let store = InMemoryEventStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let window = response_window();
    let runtime = AgentRuntime::with_event_store(
        MetadataModel::new(
            Some(identity()),
            [
                ModelOutput {
                    provider_state: None,
                    response: tool_response(),
                    usage: None,
                    provider_context: Some(window.clone()),
                    context_compacted: false,
                },
                ModelResponse::final_answer("Both records inspected.".to_owned()).into(),
            ],
        ),
        RecordingTools::new(Arc::clone(&effects)),
        ObservingContext::new(store.clone(), Arc::clone(&effects)),
        store,
        Some(3),
    );
    let mut run = runtime
        .start_run(&agent, "metadata-continuation", "Inspect the records")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].provider_context.as_ref(), Some(&window));
    assert_eq!(
        requests[1].messages,
        vec![
            ModelMessage::tool_result("first".to_owned(), "result-first".to_owned(), false),
            ModelMessage::tool_result("second".to_owned(), "result-second".to_owned(), false),
        ],
        "the native window already contains the assistant response and declarations"
    );
    let observations = runtime.context_manager().observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].effects, 0);
    let declarations = observations[0]
        .events
        .iter()
        .filter_map(|event| match &event.event {
            RunEvent::ToolCallRequested { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations, vec!["first", "second"]);
    assert!(observations[0].events.iter().any(|event| matches!(
        &event.event,
        RunEvent::ModelResponseReceived { content, .. } if content == "Inspect both records."
    )));
    assert!(!observations[0].events.iter().any(|event| matches!(
        event.event,
        RunEvent::ToolCallSucceeded { .. } | RunEvent::ToolCallFailed { .. }
    )));
    assert_eq!(effects.load(Ordering::SeqCst), 2);
}

struct PreparedNativeContext;

impl ContextManager for PreparedNativeContext {
    type Error = Infallible;

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        Ok(PreparedContext {
            request: ModelRequest {
                instructions: input.instructions.to_owned(),
                messages: Vec::new(),
                tools: input.tools.to_vec(),
                max_output_tokens: Some(128),
                provider_context: Some(response_window()),
            },
            omitted_history: None,
        })
    }
}

#[tokio::test]
async fn unsupported_or_incompatible_models_are_rejected_before_receiving_native_state() {
    let agent = Agent::new("agent", "Agent", "", "");
    let incompatible = ProviderContextIdentity {
        model: "different-model".to_owned(),
        ..identity()
    };
    for (index, configured_identity) in [None, Some(incompatible)].into_iter().enumerate() {
        let effects = Arc::new(AtomicUsize::new(0));
        let runtime = AgentRuntime::new(
            MetadataModel::new(configured_identity, []),
            RecordingTools::new(Arc::clone(&effects)),
            PreparedNativeContext,
            Some(2),
        );
        let mut run = runtime
            .start_run(&agent, format!("incompatible-{index}"), "Inspect")
            .await
            .unwrap();

        assert!(matches!(
            runtime.run_until_blocked(&agent, &mut run).await,
            Err(AgentRuntimeError::ProviderContextMismatch)
        ));
        assert!(runtime.model().requests.lock().unwrap().is_empty());
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(run.current_step(), 0);
        assert_eq!(run.status(), RunStatus::Failed);
    }
}

#[tokio::test]
async fn cancelling_a_pending_response_observer_drops_it_before_any_tool_effect() {
    let agent = Agent::new("agent", "Agent", "", "");
    let store = InMemoryEventStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut context = ObservingContext::new(store.clone(), Arc::clone(&effects));
    context.pause_observation = true;
    let dropped = Arc::clone(&context.observation_dropped);
    let runtime = AgentRuntime::with_event_store(
        MetadataModel::new(Some(identity()), [tool_response().into()]),
        RecordingTools::new(Arc::clone(&effects)),
        context,
        store,
        Some(2),
    );
    let mut run = runtime
        .start_run(&agent, "cancel-observation", "Inspect the records")
        .await
        .unwrap();
    let mut cancellation_handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        runtime.context_manager().observations.lock().unwrap().len(),
        1
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(!dropped.load(Ordering::SeqCst));
    let mut cancellation = Box::pin(runtime.cancel_run(&mut cancellation_handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());

    assert!(matches!(
        execution.as_mut().poll(&mut cx),
        Poll::Ready(Ok(RunStatus::Cancelled))
    ));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(execution);
    drop(cancellation);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(run.events(), cancellation_handle.events());
    assert_eq!(run.events().last().unwrap().event, RunEvent::RunCancelled);
    assert!(!run.events().iter().any(|event| matches!(
        event.event,
        RunEvent::ToolCallSucceeded { .. } | RunEvent::ToolCallFailed { .. }
    )));
}
