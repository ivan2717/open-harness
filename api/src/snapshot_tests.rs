use super::*;
use agent_core::agent::RunEvent;
use agent_runtime::{PendingUserInput, UserInputQueue};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    snapshot: Snapshot,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-stream-snapshot-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let detail = RunDetail {
            summary: RunSummary {
                id: "stream-test".into(),
                revision: 17,
                title: "Streaming".into(),
                prompt: "task".into(),
                status: RunStatus::Running,
                model_id: "fixture".into(),
                created_at: time::now(),
                updated_at: time::now(),
                delegation: None,
                execution_policy: Default::default(),
                compacting: false,
                reasoning_effort: None,
                reasoning_settings_version: 1,
                model_history_through: None,
                workspace_path: None,
                execution_directory: None,
                permission_mode: Default::default(),
            },
            messages: vec![ChatMessage::complete(
                "history".into(),
                "user",
                "previous history ".repeat(16_384),
            )],
            events: Vec::new(),
            pending_inputs: Vec::new(),
            pending_inputs_paused: false,
            context: None,
            error: None,
        };
        let snapshot = Snapshot::new(detail, directory.join("snapshot.json")).unwrap();
        Self {
            directory,
            snapshot,
        }
    }

    fn disk(&self) -> RunDetail {
        serde_json::from_slice(&fs::read(&self.snapshot.path).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn text(snapshot: &mut Snapshot, text: &str) {
    snapshot.emit(ModelStreamEvent::TextDelta {
        index: 0,
        text: text.into(),
    });
}

async fn changed<T: Clone>(receiver: &mut watch::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), receiver.changed())
        .await
        .expect("stream tail must publish without another delta")
        .unwrap();
    receiver.borrow_and_update().clone()
}

#[tokio::test]
async fn deltas_append_in_place_without_disk_writes_or_full_history_publications() {
    let mut fixture = Fixture::new();
    let before = fs::read(&fixture.snapshot.path).unwrap();
    let history_pointer = fixture
        .snapshot
        .read(|detail| detail.messages[0].text.as_ptr() as usize);
    // Any attempted persistence fails. Streaming must still accumulate normally.
    fs::create_dir(fixture.snapshot.path.with_extension("json.tmp")).unwrap();
    let mut revisions = fixture.snapshot.subscribe_revision();
    for _ in 0..1_000 {
        text(&mut fixture.snapshot, "x");
        fixture.snapshot.emit(ModelStreamEvent::ReasoningDelta {
            index: 1,
            text: "y".into(),
        });
    }
    let revision = changed(&mut revisions).await;
    fixture.snapshot.read(|detail| {
        assert_eq!(detail.messages[0].text.as_ptr() as usize, history_pointer);
        let streamed = detail.messages.last().unwrap();
        assert_eq!(streamed.text, "x".repeat(1_000));
        assert_eq!(
            streamed.reasoning.as_deref(),
            Some("y".repeat(1_000).as_str())
        );
        assert_eq!(revision, detail.summary.revision);
    });
    assert_eq!(fs::read(&fixture.snapshot.path).unwrap(), before);
    // With only revision subscribers, the legacy full channel was not cloned.
    assert!(
        fixture
            .snapshot
            .inner
            .lock()
            .unwrap()
            .full_subscribers
            .is_none()
    );
}

#[tokio::test]
async fn coalesces_burst_and_publishes_final_fragment_to_both_subscriber_types() {
    let mut fixture = Fixture::new();
    let mut full = fixture.snapshot.subscribe();
    let mut revisions = fixture.snapshot.subscribe_revision();
    for piece in ["first", " ", "last"] {
        text(&mut fixture.snapshot, piece);
    }
    assert!(!full.has_changed().unwrap());
    assert!(!revisions.has_changed().unwrap());
    let published = changed(&mut full).await;
    let revision = changed(&mut revisions).await;
    assert_eq!(published.messages.last().unwrap().text, "first last");
    assert_eq!(published.summary.revision, revision);
    assert!(!full.has_changed().unwrap());
    assert!(!revisions.has_changed().unwrap());
    assert_eq!(fixture.disk().messages.len(), 1);
}

#[tokio::test]
async fn interrupted_reply_is_durable_and_pending_flush_cannot_revive_old_stream() {
    let mut fixture = Fixture::new();
    let mut full = fixture.snapshot.subscribe();
    text(&mut fixture.snapshot, "partial response");
    for (sequence, event) in [
        RunEvent::ModelResponseInterrupted {
            content: "partial response".into(),
        },
        RunEvent::RunCancelled,
    ]
    .into_iter()
    .enumerate()
    {
        fixture
            .snapshot
            .event(&RunEventEnvelope {
                sequence: sequence as u64,
                step: 1,
                occurred_at: std::time::SystemTime::now(),
                event,
            })
            .unwrap();
    }
    let terminal = full.borrow_and_update().clone();
    assert_eq!(terminal.summary.status, RunStatus::Cancelled);
    assert_eq!(terminal.messages.last().unwrap().status, "interrupted");
    assert_eq!(
        fixture.disk().messages.last().unwrap().text,
        "partial response"
    );
    text(&mut fixture.snapshot, "late fragment");
    tokio::time::sleep(STREAM_PUBLISH_INTERVAL * 2).await;
    assert!(!full.has_changed().unwrap());
    assert_eq!(
        fixture.snapshot.summary().revision,
        terminal.summary.revision
    );
    assert!(fixture.snapshot.read(|detail| {
        detail
            .messages
            .iter()
            .all(|message| message.status != "streaming")
    }));
}

#[tokio::test]
async fn durable_queue_changes_publish_immediately_without_persisting_provisional_output() {
    let mut fixture = Fixture::new();
    let mut revisions = fixture.snapshot.subscribe_revision();
    let mut steering = UserInputQueue::subscribe(&fixture.snapshot);
    text(&mut fixture.snapshot, "not committed");
    fixture
        .snapshot
        .update(|detail| {
            detail.pending_inputs.push(PendingUserInput {
                id: "queued".into(),
                content: "new direction".into(),
                files: Vec::new(),
                steer: true,
                delivering: false,
                revision: 0,
            })
        })
        .unwrap();
    assert!(revisions.has_changed().unwrap());
    assert!(*steering.borrow_and_update());
    assert_eq!(fixture.disk().pending_inputs[0].content, "new direction");
    assert_eq!(fixture.disk().messages.len(), 1);
    let revision = *revisions.borrow_and_update();
    assert_eq!(fixture.snapshot.claim().unwrap().unwrap().id, "queued");
    assert!(fixture.disk().pending_inputs[0].delivering);
    assert!(!*steering.borrow_and_update());
    fixture.snapshot.acknowledge("queued").unwrap();
    assert!(fixture.disk().pending_inputs.is_empty());
    assert!(fixture.snapshot.summary().revision > revision);
}

#[tokio::test]
async fn restart_revision_exceeds_all_uncommitted_stream_updates() {
    let mut fixture = Fixture::new();
    for _ in 0..200 {
        text(&mut fixture.snapshot, "provisional");
    }
    let observed = fixture.snapshot.summary().revision;
    let saved = fixture.disk();
    assert!(saved.summary.revision > observed);
    assert_eq!(saved.messages.len(), 1);
    let restored = Snapshot::new(saved, fixture.snapshot.path.clone()).unwrap();
    assert!(restored.summary().revision > observed);
    assert_eq!(restored.get().messages.len(), 1);
    let subscriber = restored.subscribe();
    assert!(subscriber.borrow().summary.revision > observed);
}

#[tokio::test]
async fn committed_response_replaces_stream_and_terminal_state_publishes_immediately() {
    let mut fixture = Fixture::new();
    let mut revisions = fixture.snapshot.subscribe_revision();
    text(&mut fixture.snapshot, "incomplete");
    for (sequence, event) in [
        RunEvent::ModelResponseReceived {
            content: "complete answer".into(),
            response: Some(agent_runtime::ModelResponse::final_answer(
                "complete answer".into(),
            )),
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        },
        RunEvent::RunCompleted {
            answer: "complete answer".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        fixture
            .snapshot
            .event(&RunEventEnvelope {
                sequence: sequence as u64,
                step: 1,
                occurred_at: std::time::SystemTime::now(),
                event,
            })
            .unwrap();
    }
    assert!(revisions.has_changed().unwrap());
    let revision = *revisions.borrow_and_update();
    let saved = fixture.disk();
    assert_eq!(saved.summary.status, RunStatus::Completed);
    assert_eq!(saved.messages.last().unwrap().text, "complete answer");
    assert_eq!(saved.messages.last().unwrap().status, "complete");
    text(&mut fixture.snapshot, "late fragment");
    tokio::time::sleep(STREAM_PUBLISH_INTERVAL * 2).await;
    assert_eq!(fixture.snapshot.summary().revision, revision);
    assert!(!revisions.has_changed().unwrap());
}

#[tokio::test]
async fn reservation_renewal_stays_ahead_of_stream_revisions_and_filters_streams() {
    let mut fixture = Fixture::new();
    {
        let mut current = fixture.snapshot.inner.lock().unwrap();
        current.reserved_revision = current.detail.summary.revision + 2;
        persist(
            &fixture.snapshot.path,
            &current.detail,
            current.reserved_revision,
        )
        .unwrap();
    }
    text(&mut fixture.snapshot, "a");
    text(&mut fixture.snapshot, "b");
    assert!(fixture.disk().summary.revision > fixture.snapshot.summary().revision);
    assert_eq!(fixture.disk().messages.len(), 1);
    assert_eq!(fixture.snapshot.get().messages.last().unwrap().text, "ab");
}

#[tokio::test]
async fn failed_durable_write_keeps_memory_and_subscriptions_unchanged() {
    let fixture = Fixture::new();
    let full = fixture.snapshot.subscribe();
    let revisions = fixture.snapshot.subscribe_revision();
    let before = fixture.snapshot.summary().revision;
    fs::create_dir(fixture.snapshot.path.with_extension("json.tmp")).unwrap();
    assert!(
        fixture
            .snapshot
            .update(|detail| {
                detail.summary.title = "must not commit".into();
            })
            .is_err()
    );
    assert_eq!(fixture.snapshot.summary().revision, before);
    assert_eq!(fixture.snapshot.summary().title, "Streaming");
    assert!(!full.has_changed().unwrap());
    assert!(!revisions.has_changed().unwrap());
}

#[tokio::test]
async fn late_full_subscriber_receives_current_state_after_revision_only_streaming() {
    let mut fixture = Fixture::new();
    let mut revisions = fixture.snapshot.subscribe_revision();
    text(&mut fixture.snapshot, "latest");
    changed(&mut revisions).await;
    let full = fixture.snapshot.subscribe();
    assert_eq!(full.borrow().messages.last().unwrap().text, "latest");
    assert_eq!(
        full.borrow().summary.revision,
        fixture.snapshot.summary().revision
    );
}
