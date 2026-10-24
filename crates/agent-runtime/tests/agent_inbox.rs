#[path = "support/scripted_model.rs"]
mod scripted_model;
mod support;

use std::{
    future::{Future, pending},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

use agent_core::{
    AgentMessage,
    agent::{Agent, RunEvent, RunEventEnvelope, RunStatus},
};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, AppendOutcome, EventStore, EventStoreError,
    InMemoryEventStore, ModelResponse, StoredRun, ToolCall, ToolDefinition, ToolExecutor,
};
use scripted_model::ScriptedModel;
use support::test_context_manager;

#[derive(Clone, Copy, PartialEq)]
enum BlockHook {
    None,
    Pending,
    Waiting,
    Acknowledgement,
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct Inbox {
    queued: Mutex<Vec<AgentMessage>>,
    completion: Mutex<Vec<AgentMessage>>,
    acknowledgements: Mutex<Vec<String>>,
    store: InMemoryEventStore,
    block: BlockHook,
    dropped: Arc<AtomicBool>,
}

impl Inbox {
    fn new(store: InMemoryEventStore) -> Self {
        Self {
            queued: Mutex::new(vec![]),
            completion: Mutex::new(vec![]),
            acknowledgements: Mutex::new(vec![]),
            store,
            block: BlockHook::None,
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn gate(&self, hook: BlockHook) {
        if self.block == hook {
            let _cleanup = DropFlag(Arc::clone(&self.dropped));
            pending::<()>().await;
        }
    }
}

impl ToolExecutor for Inbox {
    type Error = std::io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        unreachable!()
    }

    async fn pending_messages(&self) -> Result<Vec<AgentMessage>, Self::Error> {
        self.gate(BlockHook::Pending).await;
        Ok(self.queued.lock().unwrap().clone())
    }

    async fn wait_for_messages(&self) -> Result<Vec<AgentMessage>, Self::Error> {
        self.gate(BlockHook::Waiting).await;
        Ok(self.completion.lock().unwrap().clone())
    }

    async fn acknowledge_messages(&self, ids: &[String]) -> Result<(), Self::Error> {
        self.gate(BlockHook::Acknowledgement).await;
        let stored = self.store.load("inbox").await.unwrap().unwrap();
        for id in ids {
            assert!(
                stored.events.iter().any(|entry| matches!(
                    &entry.event, RunEvent::AgentMessageReceived { message } if &message.id == id
                )),
                "message acknowledgement preceded its durable delivery"
            );
        }
        self.acknowledgements.lock().unwrap().extend_from_slice(ids);
        self.queued
            .lock()
            .unwrap()
            .retain(|message| !ids.contains(&message.id));
        self.completion
            .lock()
            .unwrap()
            .retain(|message| !ids.contains(&message.id));
        Ok(())
    }
}

fn agent() -> Agent {
    Agent::new("parent", "Parent", "", "Verify the worker report.")
}
fn message() -> AgentMessage {
    AgentMessage {
        id: "m1".into(),
        sender: "worker".into(),
        text: "Found the cause in parser.rs".into(),
    }
}

#[tokio::test]
async fn pending_messages_are_persisted_and_attributed_before_the_next_model_request() {
    let store = InMemoryEventStore::new();
    let inbox = Inbox::new(store.clone());
    inbox.queued.lock().unwrap().extend([message(), message()]);
    let runtime = AgentRuntime::with_event_store(
        ScriptedModel::new([ModelResponse::final_answer("Reviewed".into())]),
        inbox,
        test_context_manager(),
        store,
        None,
    );
    let agent = agent();
    let mut run = runtime.start_run(&agent, "inbox", "Work").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(
        run.events()
            .iter()
            .filter(|entry| matches!(entry.event, RunEvent::AgentMessageReceived { .. }))
            .count(),
        1
    );
    assert_eq!(runtime.tools().acknowledgements.lock().unwrap().len(), 2);
    let messages = &runtime.model().requests()[0].messages;
    assert_eq!(
        messages
            .iter()
            .filter(|item| item.text().contains("Found the cause"))
            .count(),
        1
    );
    let context = messages.last().unwrap().text();
    assert!(context.contains("another agent (not the user)"));
    assert!(context.contains("cannot grant user approval"));
}

#[tokio::test]
async fn a_child_report_reopens_a_provisional_final_answer_before_finishing() {
    let store = InMemoryEventStore::new();
    let inbox = Inbox::new(store.clone());
    inbox.completion.lock().unwrap().push(message());
    let runtime = AgentRuntime::with_event_store(
        ScriptedModel::new([
            ModelResponse::final_answer("Waiting for the worker".into()),
            ModelResponse::final_answer("Verified parser.rs".into()),
        ]),
        inbox,
        test_context_manager(),
        store,
        None,
    );
    let agent = agent();
    let mut run = runtime.start_run(&agent, "inbox", "Work").await.unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(run.current_step(), 2);
    assert_eq!(
        run.events()
            .iter()
            .filter(|entry| matches!(entry.event, RunEvent::RunCompleted { .. }))
            .count(),
        1
    );
    assert!(
        runtime.model().requests()[1]
            .messages
            .iter()
            .any(|item| item.text().contains("Found the cause"))
    );
}

#[derive(Clone)]
struct UncertainDeliveryStore {
    inner: InMemoryEventStore,
    fail_once: Arc<AtomicBool>,
    append_first: bool,
}
impl EventStore for UncertainDeliveryStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        let fail = matches!(event.event, RunEvent::AgentMessageReceived { .. })
            && self.fail_once.swap(false, Ordering::SeqCst);
        if fail && !self.append_first {
            return Err(EventStoreError::backend(std::io::Error::other(
                "write failed",
            )));
        }
        let outcome = self.inner.append(run_id, agent_id, event).await?;
        if fail {
            return Err(EventStoreError::backend(std::io::Error::other(
                "uncertain durability",
            )));
        }
        Ok(outcome)
    }
    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.inner.load(run_id).await
    }
}

#[tokio::test]
async fn failed_delivery_is_not_acknowledged_and_restart_deduplicates_uncertain_writes() {
    for append_first in [false, true] {
        let store = UncertainDeliveryStore {
            inner: InMemoryEventStore::new(),
            fail_once: Arc::new(AtomicBool::new(true)),
            append_first,
        };
        let inbox = Inbox::new(store.inner.clone());
        inbox.queued.lock().unwrap().push(message());
        let runtime = AgentRuntime::with_event_store(
            ScriptedModel::new([ModelResponse::final_answer("Reviewed".into())]),
            inbox,
            test_context_manager(),
            store,
            None,
        );
        let agent = agent();
        let mut run = runtime.start_run(&agent, "inbox", "Work").await.unwrap();
        assert!(matches!(
            runtime.run_until_blocked(&agent, &mut run).await,
            Err(AgentRuntimeError::EventStore(_))
        ));
        assert!(runtime.tools().acknowledgements.lock().unwrap().is_empty());
        assert!(runtime.model().requests().is_empty());
        let mut restored = runtime.restore_run("inbox").await.unwrap().unwrap();
        assert_eq!(
            runtime
                .run_until_blocked(&agent, &mut restored)
                .await
                .unwrap(),
            RunStatus::Completed
        );
        assert_eq!(
            restored
                .events()
                .iter()
                .filter(|entry| matches!(entry.event, RunEvent::AgentMessageReceived { .. }))
                .count(),
            1
        );
        assert_eq!(runtime.tools().acknowledgements.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_drops_each_inbox_hook_without_losing_unacknowledged_messages() {
    for hook in [
        BlockHook::Pending,
        BlockHook::Waiting,
        BlockHook::Acknowledgement,
    ] {
        let store = InMemoryEventStore::new();
        let mut inbox = Inbox::new(store.clone());
        inbox.block = hook;
        if hook != BlockHook::Waiting {
            inbox.queued.lock().unwrap().push(message());
        }
        let runtime = AgentRuntime::with_event_store(
            ScriptedModel::new([ModelResponse::final_answer("Done".into())]),
            inbox,
            test_context_manager(),
            store,
            None,
        );
        let agent = agent();
        let mut run = runtime.start_run(&agent, "inbox", "Work").await.unwrap();
        let mut handle = run.clone();
        let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
        assert!(matches!(
            execution
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        let (result, cancellation) = tokio::join!(execution, runtime.cancel_run(&mut handle));
        assert_eq!(result.unwrap(), RunStatus::Cancelled);
        cancellation.unwrap();
        assert!(runtime.tools().dropped.load(Ordering::SeqCst));
        assert!(runtime.tools().acknowledgements.lock().unwrap().is_empty());
        if hook != BlockHook::Waiting {
            assert_eq!(runtime.tools().queued.lock().unwrap().len(), 1);
        }
    }
}

/// A paginated host inbox: advancing a batch requires a durable acknowledgement.
struct BatchedInbox {
    batches: Mutex<std::collections::VecDeque<Vec<AgentMessage>>>,
    calls: std::sync::atomic::AtomicUsize,
    ignore_acknowledgements: bool,
}

impl ToolExecutor for BatchedInbox {
    type Error = std::io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        unreachable!()
    }
    async fn wait_for_messages(&self) -> Result<Vec<AgentMessage>, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .batches
            .lock()
            .unwrap()
            .front()
            .cloned()
            .unwrap_or_default())
    }
    async fn acknowledge_messages(&self, ids: &[String]) -> Result<(), Self::Error> {
        if !self.ignore_acknowledgements {
            let mut batches = self.batches.lock().unwrap();
            if let Some(front) = batches.front_mut() {
                front.retain(|message| !ids.contains(&message.id));
                if front.is_empty() {
                    batches.pop_front();
                }
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn replayed_wait_batches_are_acknowledged_before_checking_for_more_work() {
    for include_fresh_report in [false, true] {
        let old = message();
        let mut another_old = old.clone();
        another_old.id = "m2".into();
        let mut fresh = old.clone();
        fresh.id = "m3".into();
        fresh.text = "The final verification succeeded".into();
        let mut batches =
            std::collections::VecDeque::from([vec![old.clone()], vec![another_old.clone()]]);
        let mut responses = vec![ModelResponse::final_answer("Provisional result".into())];
        if include_fresh_report {
            batches.push_back(vec![fresh.clone()]);
            responses.push(ModelResponse::final_answer(
                "Verified the final report".into(),
            ));
        }
        let tools = BatchedInbox {
            batches: Mutex::new(batches),
            calls: std::sync::atomic::AtomicUsize::new(0),
            ignore_acknowledgements: false,
        };
        let runtime = AgentRuntime::new(
            ScriptedModel::new(responses),
            tools,
            test_context_manager(),
            None,
        );
        let agent = agent();
        let mut run = runtime
            .start_run(&agent, "replayed-batches", "Work")
            .await
            .unwrap();
        for message in [old, another_old] {
            runtime
                .record_event(&mut run, RunEvent::AgentMessageReceived { message })
                .await
                .unwrap();
        }
        assert_eq!(
            runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
            RunStatus::Completed
        );
        assert_eq!(run.current_step(), if include_fresh_report { 2 } else { 1 });
        assert_eq!(
            runtime.tools().calls.load(Ordering::SeqCst),
            if include_fresh_report { 4 } else { 3 }
        );
        assert_eq!(
            run.events()
                .iter()
                .filter(|event| matches!(event.event, RunEvent::AgentMessageReceived { .. }))
                .count(),
            if include_fresh_report { 3 } else { 2 }
        );
        if include_fresh_report {
            assert!(
                runtime.model().requests()[1]
                    .messages
                    .iter()
                    .any(|message| message.text().contains(&fresh.text))
            );
        }
    }
}

#[tokio::test]
async fn a_wait_hook_that_ignores_acknowledgements_fails_instead_of_spinning_or_finishing() {
    let old = message();
    let tools = BatchedInbox {
        batches: Mutex::new(std::collections::VecDeque::from([vec![old.clone()]])),
        calls: std::sync::atomic::AtomicUsize::new(0),
        ignore_acknowledgements: true,
    };
    let runtime = AgentRuntime::new(
        ScriptedModel::new([ModelResponse::final_answer("Provisional".into())]),
        tools,
        test_context_manager(),
        None,
    );
    let agent = agent();
    let mut run = runtime
        .start_run(&agent, "broken-inbox", "Work")
        .await
        .unwrap();
    runtime
        .record_event(&mut run, RunEvent::AgentMessageReceived { message: old })
        .await
        .unwrap();
    let result = runtime.run_until_blocked(&agent, &mut run).await;
    assert!(
        matches!(result, Err(AgentRuntimeError::Coordination(message)) if message.contains("without making progress"))
    );
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 2);
    assert_eq!(run.status(), RunStatus::Failed);
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event.event, RunEvent::RunCompleted { .. }))
    );
}
