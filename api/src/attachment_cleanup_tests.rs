use super::*;
use agent_core::{FileReference, agent::AgentRun};
use agent_runtime::EventStore;
use std::{
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "harness-attachment-cleanup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("runs")).unwrap();
        fs::create_dir(path.join("attachments")).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    async fn original(&self, name: &str, bytes: &[u8], old: bool) -> FileReference {
        let state = super::super::AppState::open_with_delay(self.0.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let reference = state
            .store_attachment(name.into(), bytes.to_vec())
            .await
            .unwrap();
        if old {
            File::open(self.path(&reference))
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(SystemTime::now() - RETENTION - Duration::from_secs(60)),
                )
                .unwrap();
        }
        reference
    }
    fn path(&self, reference: &FileReference) -> PathBuf {
        self.0
            .join("attachments")
            .join(&reference.attachment.as_ref().unwrap().id)
    }
    async fn record(&self, reference: FileReference) -> PathBuf {
        let run = AgentRun::start_with_files(
            "without-snapshot",
            "agent",
            "read original",
            vec![reference],
        );
        JsonlEventStore::new(self.0.join("events"))
            .append(run.id(), run.agent_id(), &run.events()[0])
            .await
            .unwrap();
        fs::read_dir(self.0.join("events"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn old_orphans_are_removed_and_recent_uploads_are_kept() {
    let directory = Directory::new();
    // Store recent first because opening a host also invokes the startup collector.
    let recent = directory.original("recent.bin", b"recent", false).await;
    let old = directory.original("old.bin", b"old", true).await;
    assert_eq!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .unwrap(),
        1
    );
    assert!(!directory.path(&old).exists());
    assert!(directory.path(&recent).exists());
}

#[tokio::test]
async fn committed_old_original_is_kept_without_snapshot_or_source_file() {
    let directory = Directory::new();
    let source = directory.0.join("source.txt");
    fs::write(&source, b"original evidence").unwrap();
    let original = directory
        .original("source.txt", &fs::read(&source).unwrap(), true)
        .await;
    fs::remove_file(source).unwrap();
    directory.record(original.clone()).await;
    assert_eq!(fs::read_dir(directory.0.join("runs")).unwrap().count(), 0);
    assert_eq!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read(directory.path(&original)).unwrap(),
        b"original evidence"
    );
}

#[tokio::test]
async fn corrupt_durable_history_prevents_all_deletion_and_does_not_block_startup() {
    let directory = Directory::new();
    let committed = directory
        .original("committed.txt", b"committed", false)
        .await;
    let log = directory.record(committed).await;
    let orphan = directory
        .original("abandoned.txt", b"abandoned", true)
        .await;
    fs::OpenOptions::new()
        .append(true)
        .open(log)
        .unwrap()
        .write_all(b"not json\n")
        .unwrap();
    assert!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .is_err()
    );
    assert!(directory.path(&orphan).exists());
    let state =
        super::super::AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO).await;
    assert!(state.is_ok(), "cleanup failure must not prevent launch");
    assert!(directory.path(&orphan).exists());
}

#[tokio::test]
async fn incomplete_or_unknown_logs_fail_closed_without_repairing_them() {
    let directory = Directory::new();
    let committed = directory
        .original("committed.txt", b"committed", false)
        .await;
    let log = directory.record(committed).await;
    let orphan = directory
        .original("abandoned.txt", b"abandoned", true)
        .await;
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"incomplete")
        .unwrap();
    let bytes = fs::read(&log).unwrap();
    assert!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(&log).unwrap(),
        bytes,
        "maintenance must not truncate event evidence"
    );
    assert!(directory.path(&orphan).exists());
    fs::remove_file(log).unwrap();
    fs::write(directory.0.join("events/unknown.jsonl"), b"{}\n").unwrap();
    assert!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .is_err()
    );
    assert!(directory.path(&orphan).exists());
}

#[tokio::test]
async fn later_pages_keep_references_without_loading_screenshot_sidecars() {
    use agent_core::{
        ModelImage,
        agent::{RunEvent, RunEventEnvelope},
    };
    let directory = Directory::new();
    let original = directory
        .original("later.txt", b"later evidence", true)
        .await;
    let run = AgentRun::new("paged", "agent", "test");
    let store = JsonlEventStore::new(directory.0.join("events"));
    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    for sequence in 1..=10 {
        let event = if sequence == 10 {
            RunEvent::InputReceived {
                content: "attached later".into(),
                files: vec![original.clone()],
            }
        } else {
            RunEvent::ToolCallSucceeded { call_id: format!("call-{sequence}"), output: "screenshot".into(), images: vec![ModelImage::new("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZcZkAAAAASUVORK5CYII=").unwrap()] }
        };
        store
            .append(
                run.id(),
                run.agent_id(),
                &RunEventEnvelope {
                    sequence,
                    step: 1,
                    occurred_at: SystemTime::now(),
                    event,
                },
            )
            .await
            .unwrap();
    }
    // Reference inventory must not read image bytes irrelevant to file reachability.
    fs::remove_dir_all(directory.0.join("events/images")).unwrap();
    assert!(store.load_range(run.id(), 1..2).await.is_err());
    assert_eq!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .unwrap(),
        0
    );
    assert!(directory.path(&original).exists());
}

#[tokio::test]
async fn a_snapshot_with_a_missing_or_empty_log_blocks_all_cleanup() {
    let directory = Directory::new();
    let saved = directory
        .original("saved.txt", b"saved original", false)
        .await;
    let log = directory.record(saved.clone()).await;
    let orphan = directory
        .original("orphan.txt", b"old abandoned upload", true)
        .await;
    File::open(directory.path(&saved))
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::now() - RETENTION - Duration::from_secs(60)),
        )
        .unwrap();
    // Another valid run means events/ remains present when the first log is lost.
    let other = AgentRun::new("unrelated", "agent", "unrelated task");
    JsonlEventStore::new(directory.0.join("events"))
        .append(other.id(), other.agent_id(), &other.events()[0])
        .await
        .unwrap();
    fs::write(
        directory.0.join("runs/without-snapshot.json"),
        serde_json::to_vec(&serde_json::json!({
            "summary": { "id": "without-snapshot" },
            "messages": [{ "files": [saved.clone()] }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::remove_file(&log).unwrap();
    assert!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .is_err()
    );
    assert!(directory.path(&saved).exists());
    assert!(directory.path(&orphan).exists());
    fs::write(&log, b"").unwrap();
    assert!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .is_err()
    );
    assert!(directory.path(&saved).exists());
    assert!(directory.path(&orphan).exists());
    directory.record(saved.clone()).await;
    assert_eq!(
        collect(directory.0.clone(), SystemTime::now())
            .await
            .unwrap(),
        1
    );
    assert!(directory.path(&saved).exists());
    assert!(!directory.path(&orphan).exists());
}

#[tokio::test]
async fn snapshot_identity_and_stamp_are_part_of_the_complete_inventory() {
    let directory = Directory::new();
    let saved = directory.original("saved.txt", b"saved", false).await;
    directory.record(saved).await;
    let snapshot = directory.0.join("runs/without-snapshot.json");
    fs::write(
        &snapshot,
        br#"{"summary":{"id":"without-snapshot"},"messages":[]}"#,
    )
    .unwrap();
    let before = inventory(&directory.0).unwrap();
    fs::write(
        &snapshot,
        br#"{"summary":{"id":"without-snapshot"},"messages":["changed"]}"#,
    )
    .unwrap();
    assert_ne!(inventory(&directory.0).unwrap(), before);
    fs::write(&snapshot, br#"{"summary":{"id":"different"}}"#).unwrap();
    assert!(inventory(&directory.0).is_err());
    fs::write(
        &snapshot,
        br#"{"summary":{"id":"without-snapshot"},"messages":broken}"#,
    )
    .unwrap();
    assert!(inventory(&directory.0).is_err());
}
