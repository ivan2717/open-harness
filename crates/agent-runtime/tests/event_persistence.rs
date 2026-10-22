#[path = "support/scripted_model.rs"]
mod scripted_model;
mod support;

use std::{
    convert::Infallible,
    fs,
    future::Future,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{Agent, RunEvent, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, AppendOutcome, EventStore, EventStoreError, InMemoryEventStore, JsonlEventStore,
    ModelResponse, RunEventReceiver, ToolCall, ToolDefinition, ToolExecutor,
};
use tokio::sync::broadcast::error::TryRecvError;

use scripted_model::ScriptedModel;
use support::test_context_manager;

struct NoTools;

impl ToolExecutor for NoTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }

    async fn execute(&self, _call: ToolCall) -> Result<String, Self::Error> {
        unreachable!("the scripted model never requests a tool")
    }
}

#[derive(Clone)]
struct AppendThenFailStore {
    inner: InMemoryEventStore,
    fail_sequence: u64,
}

impl AppendThenFailStore {
    fn new(fail_sequence: u64) -> Self {
        Self {
            inner: InMemoryEventStore::new(),
            fail_sequence,
        }
    }
}

impl EventStore for AppendThenFailStore {
    fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> impl Future<Output = Result<AppendOutcome, EventStoreError>> + Send {
        let inner = self.inner.clone();
        let fail_sequence = self.fail_sequence;
        let run_id = run_id.to_owned();
        let agent_id = agent_id.to_owned();
        let event = event.clone();

        async move {
            let outcome = inner.append(&run_id, &agent_id, &event).await?;
            if event.sequence == fail_sequence {
                return Err(EventStoreError::backend(std::io::Error::other(
                    "durability confirmation failed",
                )));
            }
            Ok(outcome)
        }
    }

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<agent_runtime::StoredRun>, EventStoreError>> + Send
    {
        let inner = self.inner.clone();
        let run_id = run_id.to_owned();
        async move { inner.load(&run_id).await }
    }
}

fn test_agent() -> Agent {
    Agent::new("agent-1", "Agent", "Test agent", "Be precise.")
}

fn drain(receiver: &mut RunEventReceiver) -> Vec<agent_runtime::RunEventNotification> {
    let mut notifications = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(notification) => notifications.push(notification),
            Err(TryRecvError::Empty | TryRecvError::Closed) => return notifications,
            Err(TryRecvError::Lagged(count)) => {
                panic!("test subscriber unexpectedly lagged by {count} events")
            }
        }
    }
}

#[tokio::test]
async fn event_store_retries_are_idempotent_but_conflicts_are_rejected() {
    let agent = test_agent();
    let run = agent.start_run("run-idempotent", "Test duplicate events");
    let event = &run.events()[0];
    let store = InMemoryEventStore::new();

    assert_eq!(
        store.append(run.id(), run.agent_id(), event).await.unwrap(),
        AppendOutcome::Appended
    );
    assert_eq!(
        store.append(run.id(), run.agent_id(), event).await.unwrap(),
        AppendOutcome::AlreadyPresent
    );
    assert_eq!(store.load(run.id()).await.unwrap().unwrap().events.len(), 1);

    let mut conflicting = event.clone();
    conflicting.event = RunEvent::RunCancelled;
    let error = store
        .append(run.id(), run.agent_id(), &conflicting)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EventStoreError::SequenceConflict {
            run_id,
            expected: 1,
            actual: 0,
        } if run_id == "run-idempotent"
    ));
}

#[tokio::test]
async fn runtime_persists_notifies_and_restores_every_event() {
    let agent = test_agent();
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![ModelResponse::final_answer("Done".to_owned())]),
        NoTools,
        test_context_manager(),
        None,
    );
    let mut notifications = runtime.subscribe();
    let mut run = runtime
        .start_run(&agent, "run-1", "Complete the task")
        .await
        .unwrap();

    let status = runtime.run_until_blocked(&agent, &mut run).await.unwrap();

    assert_eq!(status, RunStatus::Completed);
    assert_eq!(runtime.model().requests().len(), 1);
    let stored = runtime.event_store().load("run-1").await.unwrap().unwrap();
    assert_eq!(stored.events, run.events());

    let received = drain(&mut notifications);
    assert_eq!(received.len(), run.events().len());
    assert!(received.iter().all(|event| event.run_id == "run-1"));
    assert!(received.iter().all(|event| event.agent_id == "agent-1"));
    assert_eq!(
        received
            .iter()
            .map(|event| event.envelope.clone())
            .collect::<Vec<_>>(),
        run.events()
    );

    let restored = runtime.restore_run("run-1").await.unwrap().unwrap();
    assert_eq!(restored.status(), RunStatus::Completed);
    assert_eq!(restored.current_step(), 1);
    assert_eq!(restored.events(), run.events());

    let (catch_up, _live) = runtime.subscribe_from("run-1", Some(1)).await.unwrap();
    assert_eq!(
        catch_up
            .iter()
            .map(|event| event.envelope.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[tokio::test]
async fn submitted_input_is_persisted_before_the_run_resumes() {
    let agent = test_agent();
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::input_required("Which repository?".to_owned()),
            ModelResponse::final_answer("I will inspect open-harness.".to_owned()),
        ]),
        NoTools,
        test_context_manager(),
        Some(4),
    );
    let mut notifications = runtime.subscribe();
    let mut run = runtime
        .start_run(&agent, "run-input", "Inspect a repository")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );

    runtime
        .submit_input(&mut run, "open-harness")
        .await
        .unwrap();

    let stored = runtime
        .event_store()
        .load("run-input")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &stored.events.last().unwrap().event,
        RunEvent::InputReceived { content } if content == "open-harness"
    ));
    assert_eq!(run.status(), RunStatus::Running);

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );

    let received = drain(&mut notifications);
    assert_eq!(received.len(), run.events().len());
    assert!(received.iter().any(|notification| matches!(
        &notification.envelope.event,
        RunEvent::InputReceived { content } if content == "open-harness"
    )));
}

#[tokio::test]
async fn jsonl_store_restores_a_run_from_disk() {
    let directory = TestDirectory::new();
    let agent = test_agent();
    let run = {
        let runtime = AgentRuntime::with_event_store(
            ScriptedModel::new(vec![ModelResponse::final_answer("Persisted".to_owned())]),
            NoTools,
            test_context_manager(),
            JsonlEventStore::new(directory.path()),
            None,
        );
        let mut run = runtime
            .start_run(&agent, "run-on-disk", "Persist this run")
            .await
            .unwrap();
        runtime.run_until_blocked(&agent, &mut run).await.unwrap();
        run
    };

    let runtime = AgentRuntime::with_event_store(
        ScriptedModel::new(Vec::new()),
        NoTools,
        test_context_manager(),
        JsonlEventStore::new(directory.path()),
        None,
    );
    let restored = runtime.restore_run("run-on-disk").await.unwrap().unwrap();

    assert_eq!(restored.status(), RunStatus::Completed);
    assert_eq!(restored.events(), run.events());

    let files: Vec<_> = fs::read_dir(directory.path()).unwrap().collect();
    assert_eq!(files.len(), 1);
    let contents = fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(contents.contains("\"schema_version\":1"));
    assert!(contents.contains("\"type\":\"run_started\""));
}

#[tokio::test]
async fn append_errors_are_not_hidden_by_reading_the_event_back() {
    let agent = test_agent();
    let store = AppendThenFailStore::new(1);
    let runtime = AgentRuntime::with_event_store(
        ScriptedModel::new(vec![ModelResponse::final_answer(
            "This response must not run".to_owned(),
        )]),
        NoTools,
        test_context_manager(),
        store,
        None,
    );
    let mut notifications = runtime.subscribe();
    let mut run = runtime
        .start_run(&agent, "run-uncertain", "Test uncertain durability")
        .await
        .unwrap();

    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        agent_runtime::AgentRuntimeError::EventStore(_)
    ));
    assert_eq!(run.events().len(), 1);
    assert_eq!(drain(&mut notifications).len(), 1);

    let stored = runtime
        .event_store()
        .load("run-uncertain")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.events.len(), 2);
    assert!(matches!(
        stored.events.last().unwrap().event,
        RunEvent::ModelRequestStarted
    ));

    let restored = runtime.restore_run("run-uncertain").await.unwrap().unwrap();
    assert_eq!(restored.status(), RunStatus::Running);
    assert_eq!(restored.events(), stored.events);
}

#[tokio::test]
async fn subscribe_from_filters_live_events_by_run_id() {
    let agent = test_agent();
    let runtime = AgentRuntime::new(
        ScriptedModel::new(vec![
            ModelResponse::final_answer("run-b done".to_owned()),
            ModelResponse::final_answer("run-a done".to_owned()),
        ]),
        NoTools,
        test_context_manager(),
        None,
    );
    let mut run_a = runtime
        .start_run(&agent, "run-a", "Complete A")
        .await
        .unwrap();
    let (catch_up, mut run_a_events) = runtime.subscribe_from("run-a", Some(0)).await.unwrap();
    assert!(catch_up.is_empty());

    let mut run_b = runtime
        .start_run(&agent, "run-b", "Complete B")
        .await
        .unwrap();
    runtime.run_until_blocked(&agent, &mut run_b).await.unwrap();
    runtime.run_until_blocked(&agent, &mut run_a).await.unwrap();

    let received = drain(&mut run_a_events);
    assert_eq!(
        received
            .iter()
            .map(|notification| notification.envelope.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(
        received
            .iter()
            .all(|notification| notification.run_id == "run-a")
    );
}

#[tokio::test]
async fn jsonl_store_recovers_an_empty_file() {
    let directory = TestDirectory::new();
    let agent = test_agent();
    let run = agent.start_run("run-empty", "Recover an empty log");
    let event = &run.events()[0];
    let store = JsonlEventStore::new(directory.path());

    store.append(run.id(), run.agent_id(), event).await.unwrap();
    let path = only_event_file(directory.path());
    fs::write(&path, []).unwrap();

    assert!(store.load(run.id()).await.unwrap().is_none());
    assert_eq!(
        store.append(run.id(), run.agent_id(), event).await.unwrap(),
        AppendOutcome::Appended
    );
    assert_eq!(
        store.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
}

#[tokio::test]
async fn jsonl_store_discards_an_unterminated_record() {
    let directory = TestDirectory::new();
    let agent = test_agent();
    let run = agent.start_run("run-unterminated", "Recover an unterminated record");
    let event = &run.events()[0];
    let store = JsonlEventStore::new(directory.path());

    store.append(run.id(), run.agent_id(), event).await.unwrap();
    let path = only_event_file(directory.path());
    let mut bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    fs::write(&path, bytes).unwrap();

    assert!(store.load(run.id()).await.unwrap().is_none());
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(
        store.append(run.id(), run.agent_id(), event).await.unwrap(),
        AppendOutcome::Appended
    );
}

#[tokio::test]
async fn jsonl_store_truncates_a_partial_tail_before_appending() {
    let directory = TestDirectory::new();
    let agent = test_agent();
    let mut run = agent.start_run("run-torn-tail", "Recover a partial tail");
    let store = JsonlEventStore::new(directory.path());

    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    let path = only_event_file(directory.path());
    let committed_length = fs::metadata(&path).unwrap().len();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"schema_version\":1,\"run_id\":")
        .unwrap();

    let stored = store.load(run.id()).await.unwrap().unwrap();
    assert_eq!(stored.events, run.events());
    assert_eq!(fs::metadata(&path).unwrap().len(), committed_length);

    run.begin_step().unwrap();
    store
        .append(run.id(), run.agent_id(), &run.events()[1])
        .await
        .unwrap();
    assert_eq!(
        store.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
}

#[tokio::test]
async fn jsonl_store_does_not_hide_a_terminated_corrupt_record() {
    let directory = TestDirectory::new();
    let agent = test_agent();
    let mut run = agent.start_run("run-corrupt", "Reject committed corruption");
    let store = JsonlEventStore::new(directory.path());

    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    let path = only_event_file(directory.path());
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{broken}\n")
        .unwrap();
    let corrupt_length = fs::metadata(&path).unwrap().len();

    let error = store.load(run.id()).await.unwrap_err();
    assert!(matches!(
        error,
        EventStoreError::CorruptEvent { line: 2, .. }
    ));
    assert_eq!(fs::metadata(&path).unwrap().len(), corrupt_length);

    run.begin_step().unwrap();
    let error = store
        .append(run.id(), run.agent_id(), &run.events()[1])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EventStoreError::CorruptEvent { line: 2, .. }
    ));
    assert_eq!(fs::metadata(&path).unwrap().len(), corrupt_length);
}

async fn assert_range_reads(store: &impl EventStore) {
    let agent = test_agent();
    let mut run = agent.start_run("run-ranges", "Load a bounded history");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Done".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::RunCompleted {
        answer: "Done".to_owned(),
    })
    .unwrap();
    for event in run.events() {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }

    let range = store.load_range(run.id(), 1..3).await.unwrap().unwrap();
    assert_eq!(range.run_id, run.id());
    assert_eq!(range.agent_id, run.agent_id());
    assert_eq!(range.events, run.events()[1..3]);
    assert_eq!(
        store
            .load_range(run.id(), 2..u64::MAX)
            .await
            .unwrap()
            .unwrap()
            .events,
        run.events()[2..]
    );
    for range in [0..0, 10..20, std::ops::Range { start: 3, end: 1 }] {
        assert!(
            store
                .load_range(run.id(), range)
                .await
                .unwrap()
                .unwrap()
                .events
                .is_empty()
        );
    }
    assert!(
        store
            .load_range("missing-run", 0..1)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn memory_and_jsonl_range_reads_match_the_compatible_default() {
    assert_range_reads(&InMemoryEventStore::new()).await;
    assert_range_reads(&AppendThenFailStore::new(u64::MAX)).await;
    let directory = TestDirectory::new();
    assert_range_reads(&JsonlEventStore::new(directory.path())).await;

    // A newly constructed store must index the existing log before seeking.
    let reopened = JsonlEventStore::new(directory.path());
    assert_eq!(
        reopened
            .load_range("run-ranges", 2..3)
            .await
            .unwrap()
            .unwrap()
            .events[0]
            .sequence,
        2
    );
}

async fn assert_tool_call_lookups(store: &impl EventStore) {
    let run = test_agent().start_run("run-tool-lookup", "Resolve tool request metadata");
    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    // A store is a log primitive: unlike AgentRun, it also preserves duplicate
    // raw IDs imported from older histories, including all matching requests.
    let events = [
        RunEvent::ModelRequestStarted,
        RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "Read files".to_owned(),
        },
        RunEvent::ToolCallRequested {
            call_id: "duplicate".to_owned(),
            tool_name: "read_run_history".to_owned(),
            arguments: "{}".to_owned(),
        },
        RunEvent::ToolCallSucceeded {
            call_id: "duplicate".to_owned(),
            output: "large unrelated output".repeat(1_000),
        },
        RunEvent::ToolCallRequested {
            call_id: "second".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        },
        RunEvent::ToolCallSucceeded {
            call_id: "second".to_owned(),
            output: "another large output".repeat(1_000),
        },
        RunEvent::ToolCallRequested {
            call_id: "duplicate".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        },
    ];
    for (index, event) in events.into_iter().enumerate() {
        store
            .append(
                run.id(),
                run.agent_id(),
                &RunEventEnvelope {
                    sequence: index as u64 + 1,
                    step: 1,
                    occurred_at: SystemTime::now(),
                    event,
                },
            )
            .await
            .unwrap();
    }
    let requests = store
        .load_tool_calls(
            run.id(),
            &[
                "second".to_owned(),
                "duplicate".to_owned(),
                "duplicate".to_owned(),
                "missing".to_owned(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 5, 7]
    );
    assert!(
        requests
            .iter()
            .all(|envelope| matches!(envelope.event, RunEvent::ToolCallRequested { .. }))
    );
    assert!(
        store
            .load_tool_calls(run.id(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .load_tool_calls("missing-run", &["second".to_owned()])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn indexed_tool_call_lookups_match_the_compatible_default() {
    assert_tool_call_lookups(&InMemoryEventStore::new()).await;
    assert_tool_call_lookups(&AppendThenFailStore::new(u64::MAX)).await;
    let directory = TestDirectory::new();
    assert_tool_call_lookups(&JsonlEventStore::new(directory.path())).await;
    // Rebuilding the index from disk must preserve duplicate request IDs too.
    let reopened = JsonlEventStore::new(directory.path());
    let requests = reopened
        .load_tool_calls("run-tool-lookup", &["duplicate".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 7]
    );
}

#[tokio::test]
async fn jsonl_index_preserves_retry_and_conflict_checks_for_old_events() {
    let directory = TestDirectory::new();
    let store = JsonlEventStore::new(directory.path());
    let agent = test_agent();
    let mut run = agent.start_run("run-indexed-retry", "Retry an old event");
    run.begin_step().unwrap();
    for event in run.events() {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
    assert_eq!(
        store
            .append(run.id(), run.agent_id(), &run.events()[0])
            .await
            .unwrap(),
        AppendOutcome::AlreadyPresent
    );
    let mut conflict = run.events()[0].clone();
    conflict.event = RunEvent::RunStarted {
        instruction: "Different event at the same sequence".to_owned(),
    };
    assert!(matches!(
        store.append(run.id(), run.agent_id(), &conflict).await,
        Err(EventStoreError::SequenceConflict {
            expected: 2,
            actual: 0,
            ..
        })
    ));
    assert!(matches!(
        store
            .append(run.id(), "wrong-agent", &run.events()[0])
            .await,
        Err(EventStoreError::AgentMismatch { .. })
    ));
    assert_eq!(
        store.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
}

#[tokio::test]
async fn jsonl_clones_serialize_conflicting_appends_to_the_same_run() {
    let directory = TestDirectory::new();
    let store = JsonlEventStore::new(directory.path());
    let other = store.clone();
    let agent = test_agent();
    let run = agent.start_run("run-concurrent", "Serialize duplicate appends");
    let event = &run.events()[0];
    let (first, second) = tokio::join!(
        store.append(run.id(), run.agent_id(), event),
        other.append(run.id(), run.agent_id(), event)
    );
    assert!(matches!(
        (first.unwrap(), second.unwrap()),
        (AppendOutcome::Appended, AppendOutcome::AlreadyPresent)
            | (AppendOutcome::AlreadyPresent, AppendOutcome::Appended)
    ));
    assert_eq!(store.load(run.id()).await.unwrap().unwrap().events.len(), 1);
}

#[tokio::test]
async fn jsonl_index_rechecks_same_length_external_modifications() {
    let directory = TestDirectory::new();
    let store = JsonlEventStore::new(directory.path());
    let agent = test_agent();
    let mut run = agent.start_run("run-rewritten", "Detect committed corruption");
    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    let path = only_event_file(directory.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'[';
    fs::write(&path, &bytes).unwrap();
    // Explicitly change mtime even on filesystems with coarse timestamp resolution.
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(7))
        .unwrap();

    run.begin_step().unwrap();
    assert!(matches!(
        store
            .append(run.id(), run.agent_id(), &run.events()[1])
            .await,
        Err(EventStoreError::CorruptEvent { line: 1, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

fn only_event_file(directory: &Path) -> PathBuf {
    let mut entries = fs::read_dir(directory).unwrap();
    let path = entries.next().unwrap().unwrap().path();
    assert!(entries.next().is_none());
    path
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        loop {
            let counter = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "open-harness-event-store-{}-{unique}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("failed to create test directory: {error}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
