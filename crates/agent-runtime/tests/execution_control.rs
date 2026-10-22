mod support;

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
    AgentRuntime, AgentRuntimeError, AppendOutcome, EventStore, EventStoreError,
    InMemoryEventStore, Model, ModelRequest, ModelResponse, StoredRun, ToolCall, ToolDefinition,
    ToolExecutor,
};
use support::test_context_manager;
use tokio::sync::Notify;

/// Runs a finite response script, then waits until execution is cancelled.
struct PendingAfterScriptModel(Mutex<VecDeque<ModelResponse>>);

impl Model for PendingAfterScriptModel {
    type Error = Infallible;

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        let response = self.0.lock().unwrap().pop_front();
        match response {
            Some(response) => Ok(response),
            None => pending().await,
        }
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct GatedTools {
    release: Notify,
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
    effects: AtomicUsize,
    definitions: Vec<ToolDefinition>,
}

impl GatedTools {
    fn new() -> Self {
        Self {
            release: Notify::new(),
            dropped: Arc::new(AtomicBool::new(false)),
            calls: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            definitions: vec![ToolDefinition {
                name: "write".into(),
                description: "A cancellable external operation".into(),
                input_schema: "{}".into(),
            }],
        }
    }
}

impl ToolExecutor for GatedTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    fn execute(&self, _: ToolCall) -> impl Future<Output = Result<String, Self::Error>> + Send {
        // Deliberately outside the async block: cancellation must also prevent
        // synchronous work performed while a provider constructs its future.
        self.calls.fetch_add(1, Ordering::SeqCst);
        async {
            let _cleanup = DropFlag(Arc::clone(&self.dropped));
            self.release.notified().await;
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok("written".into())
        }
    }
}

fn tool_response() -> ModelResponse {
    ModelResponse::tool_calls(
        "Writing".into(),
        vec![ToolCall {
            id: "c1".into(),
            name: "write".into(),
            arguments: "{}".into(),
        }],
    )
}

#[tokio::test]
async fn cancellation_waits_for_tool_cleanup_and_prevents_a_ready_side_effect() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::new(
        PendingAfterScriptModel(Mutex::new(VecDeque::from([tool_response()]))),
        GatedTools::new(),
        test_context_manager(),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "cancel-tool", "write")
        .await
        .unwrap();
    let mut cancellation_handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);

    let mut cancellation = Box::pin(runtime.cancel_run(&mut cancellation_handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(!runtime.tools().dropped.load(Ordering::SeqCst));
    runtime.tools().release.notify_one();
    assert!(matches!(
        execution.as_mut().poll(&mut cx),
        Poll::Ready(Ok(RunStatus::Cancelled))
    ));
    assert!(runtime.tools().dropped.load(Ordering::SeqCst));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(execution);
    drop(cancellation);
    assert_eq!(runtime.tools().effects.load(Ordering::SeqCst), 0);
    assert_eq!(run.status(), RunStatus::Cancelled);
    assert_eq!(run.events(), cancellation_handle.events());
    assert_eq!(
        run.events()
            .iter()
            .filter(|e| matches!(e.event, RunEvent::RunCancelled))
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_stops_a_pending_model_and_is_idempotent() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::new(
        PendingAfterScriptModel(Mutex::new(VecDeque::new())),
        GatedTools::new(),
        test_context_manager(),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "cancel-model", "wait")
        .await
        .unwrap();
    assert_eq!(run.status(), RunStatus::Running);
    assert!(!runtime.is_executing(run.id()));
    assert!(!runtime.is_executing("unknown-run"));
    let mut handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert!(runtime.is_executing("cancel-model"));
    assert_eq!(
        runtime
            .restore_run("cancel-model")
            .await
            .unwrap()
            .unwrap()
            .status(),
        RunStatus::Running
    );
    let mut cancellation = Box::pin(runtime.record_event(&mut handle, RunEvent::RunCancelled));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(runtime.is_executing("cancel-model"));
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
    assert!(!runtime.is_executing("cancel-model"));
    assert_eq!(
        runtime
            .restore_run("cancel-model")
            .await
            .unwrap()
            .unwrap()
            .status(),
        RunStatus::Cancelled
    );
    let event_count = handle.events().len();
    runtime.cancel_run(&mut handle).await.unwrap();
    assert_eq!(handle.events().len(), event_count);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_second_driver_is_rejected_before_it_can_execute_the_same_run() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::new(
        PendingAfterScriptModel(Mutex::new(VecDeque::new())),
        GatedTools::new(),
        test_context_manager(),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "exclusive", "wait")
        .await
        .unwrap();
    let mut other = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        runtime.run_until_blocked(&agent, &mut other).await,
        Err(AgentRuntimeError::RunAlreadyExecuting { .. })
    ));
    drop(execution);
    assert!(!runtime.is_executing("exclusive"));
    assert_eq!(
        runtime
            .restore_run("exclusive")
            .await
            .unwrap()
            .unwrap()
            .status(),
        RunStatus::Running
    );
    runtime.cancel_run(&mut other).await.unwrap();
    assert_eq!(other.status(), RunStatus::Cancelled);
}

#[tokio::test]
async fn a_wrong_agent_handle_cannot_cancel_an_active_run() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::new(
        PendingAfterScriptModel(Mutex::new(VecDeque::new())),
        GatedTools::new(),
        test_context_manager(),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "wrong-agent", "wait")
        .await
        .unwrap();
    let mut wrong = Agent::new("other", "", "", "").start_run("wrong-agent", "wait");
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        runtime.cancel_run(&mut wrong).await,
        Err(AgentRuntimeError::AgentMismatch { .. })
    ));
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert!(
        !runtime
            .event_store()
            .load("wrong-agent")
            .await
            .unwrap()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e.event, RunEvent::RunCancelled))
    );
}

#[tokio::test]
async fn a_completed_tool_batch_can_resume_without_replaying_its_side_effects() {
    let agent = Agent::new("a", "a", "", "");
    let store = InMemoryEventStore::new();
    let mut historical = agent.start_run("resume", "write and explain");
    historical.begin_step().unwrap();
    historical
        .record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "Writing".into(),
        })
        .unwrap();
    historical
        .record(RunEvent::ToolCallRequested {
            call_id: "c1".into(),
            tool_name: "write".into(),
            arguments: "{}".into(),
        })
        .unwrap();
    historical
        .record(RunEvent::ToolCallSucceeded {
            call_id: "c1".into(),
            output: "written".into(),
        })
        .unwrap();
    for event in historical.events() {
        store
            .append(historical.id(), historical.agent_id(), event)
            .await
            .unwrap();
    }
    let runtime = AgentRuntime::with_event_store(
        PendingAfterScriptModel(Mutex::new(VecDeque::from([ModelResponse::final_answer(
            "Done".into(),
        )]))),
        GatedTools::new(),
        test_context_manager(),
        store,
        Some(4),
    );
    let mut run = runtime.restore_run("resume").await.unwrap().unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
    assert_eq!(run.current_step(), 2);
}

struct GatedStore {
    inner: InMemoryEventStore,
    release: Notify,
}

impl EventStore for GatedStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        let outcome = self.inner.append(run_id, agent_id, event).await?;
        if matches!(event.event, RunEvent::ToolCallRequested { .. }) {
            self.release.notified().await;
        }
        Ok(outcome)
    }

    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.inner.load(run_id).await
    }
}

#[tokio::test]
async fn cancellation_does_not_interrupt_an_in_flight_persistence_operation() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::with_event_store(
        PendingAfterScriptModel(Mutex::new(VecDeque::from([tool_response()]))),
        GatedTools::new(),
        test_context_manager(),
        GatedStore {
            inner: InMemoryEventStore::new(),
            release: Notify::new(),
        },
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "cancel-commit", "write")
        .await
        .unwrap();
    let mut handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    let mut cancellation = Box::pin(runtime.cancel_run(&mut handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
    runtime.event_store().release.notify_one();
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
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
    assert_eq!(run.events(), handle.events());
    assert_eq!(run.events().last().unwrap().event, RunEvent::RunCancelled);
}

struct UncertainCancelStore(InMemoryEventStore);

impl EventStore for UncertainCancelStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        let outcome = self.0.append(run_id, agent_id, event).await?;
        if matches!(event.event, RunEvent::RunCancelled) {
            return Err(EventStoreError::Backend(Box::new(std::io::Error::other(
                "durability unavailable",
            ))));
        }
        Ok(outcome)
    }

    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.0.load(run_id).await
    }
}

#[tokio::test]
async fn cancellation_does_not_acknowledge_a_readable_but_unconfirmed_terminal_write() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::with_event_store(
        PendingAfterScriptModel(Mutex::new(VecDeque::new())),
        GatedTools::new(),
        test_context_manager(),
        UncertainCancelStore(InMemoryEventStore::new()),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "uncertain-cancel", "wait")
        .await
        .unwrap();
    let mut handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    let mut cancellation = Box::pin(runtime.cancel_run(&mut handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        execution.as_mut().poll(&mut cx),
        Poll::Ready(Err(AgentRuntimeError::EventStore(_)))
    ));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(Err(AgentRuntimeError::EventStore(_)))
    ));
    assert_eq!(
        runtime
            .event_store()
            .load("uncertain-cancel")
            .await
            .unwrap()
            .unwrap()
            .events
            .last()
            .unwrap()
            .event,
        RunEvent::RunCancelled
    );
}

struct EagerTools {
    definitions: Vec<ToolDefinition>,
    calls: AtomicUsize,
}

impl ToolExecutor for EagerTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("done".into())
    }
}

#[tokio::test]
async fn immediately_ready_tools_yield_so_cancellation_can_stop_the_next_effect() {
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::new(
        PendingAfterScriptModel(Mutex::new(VecDeque::from([ModelResponse::tool_calls(
            "".into(),
            (1..=2)
                .map(|id| ToolCall {
                    id: format!("c{id}"),
                    name: "write".into(),
                    arguments: "{}".into(),
                })
                .collect(),
        )]))),
        EagerTools {
            definitions: GatedTools::new().definitions,
            calls: AtomicUsize::new(0),
        },
        test_context_manager(),
        Some(4),
    );
    let mut run = runtime
        .start_run(&agent, "ready-tools", "write twice")
        .await
        .unwrap();
    let mut handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
    let mut cancellation = Box::pin(runtime.cancel_run(&mut handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        execution.as_mut().poll(&mut cx),
        Poll::Ready(Ok(RunStatus::Cancelled))
    ));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
}
