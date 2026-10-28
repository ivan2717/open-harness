use super::*;
use crate::{
    contracts::{CreateRunInput, Delegation, RunDetail, RunStatus, RunSummary},
    demo::MODEL_ID,
};
use std::time::Duration;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "open-harness-attachments-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn originals_are_immutable_deduplicated_and_independent_of_source() {
    let directory = Directory::new();
    let source = directory.0.join("source.txt");
    let content = "evidence\n".repeat(40_000).into_bytes();
    assert!(content.len() > 256 * 1024);
    fs::write(&source, &content).unwrap();
    let reference = write_stored(
        &directory.0,
        "source.txt".into(),
        &fs::read(&source).unwrap(),
    )
    .unwrap();
    fs::remove_file(&source).unwrap();
    let duplicate = write_stored(&directory.0, "renamed.txt".into(), &content).unwrap();
    assert_eq!(reference.attachment, duplicate.attachment);
    assert_eq!(
        fs::read_dir(directory.0.join("attachments"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        read_stored(&directory.0, reference.attachment.as_ref().unwrap())
            .unwrap()
            .1,
        content
    );
    assert!(reference.text.is_empty());
    let prompt = agent_core::format_user_input("Review this", &[reference]);
    assert!(prompt.len() < 2000);
    assert!(!prompt.contains("evidence\n"));
    assert!(prompt.contains("attachment:"));
}

#[test]
fn rejects_invalid_ids_mixed_payloads_size_spoofing_and_tampered_content() {
    let directory = Directory::new();
    let reference = write_stored(&directory.0, "notes.txt".into(), b"original").unwrap();
    let stored = reference.attachment.as_ref().unwrap();
    for id in ["../notes.txt".to_string(), "A".repeat(64), "a".repeat(63)] {
        assert!(read_stored(&directory.0, &AttachmentReference { id, size: 8 }).is_err());
    }
    assert!(write_stored(&directory.0, "../notes.txt".into(), b"x").is_err());
    let mut spoofed = stored.clone();
    spoofed.size += 1;
    assert!(read_stored(&directory.0, &spoofed).is_err());
    let mut mixed = reference.clone();
    mixed.text = "hidden inline data".into();
    assert!(crate::input::validate("review", &[mixed]).is_err());
    fs::write(
        directory.0.join("attachments").join(&stored.id),
        b"tampered",
    )
    .unwrap();
    assert!(read_stored(&directory.0, stored).is_err());
    // A duplicate upload cannot silently accept a modified existing object.
    assert!(write_stored(&directory.0, "notes.txt".into(), b"original").is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_directories_and_symlink_originals() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let outside = Directory::new();
    symlink(&outside.0, directory.0.join("attachments")).unwrap();
    assert!(write_stored(&directory.0, "notes.txt".into(), b"secret").is_err());
    fs::remove_file(directory.0.join("attachments")).unwrap();
    let reference = write_stored(&directory.0, "notes.txt".into(), b"secret").unwrap();
    let attachment = reference.attachment.unwrap();
    let path = directory.0.join("attachments").join(&attachment.id);
    fs::remove_file(&path).unwrap();
    fs::write(outside.0.join("data"), b"secret").unwrap();
    symlink(outside.0.join("data"), path).unwrap();
    assert!(read_stored(&directory.0, &attachment).is_err());
}

async fn record_run(state: &AppState, id: &str, files: Vec<FileReference>, root: Option<&str>) {
    let detail = RunDetail {
        summary: RunSummary {
            reasoning_settings_version: 1,
            reasoning_effort: None,
            model_history_through: None,
            delegation: root.map(|root| Delegation {
                parent_run_id: root.into(),
                root_run_id: root.into(),
                task_name: id.into(),
                write_paths: None,
                model_override: None,
                spawn_call_id: format!("spawn-{id}"),
            }),
            execution_policy: Default::default(),
            compacting: false,
            id: id.into(),
            revision: 0,
            title: "Attachment test".into(),
            prompt: "Review".into(),
            status: RunStatus::Running,
            model_id: MODEL_ID.into(),
            workspace_path: None,
            execution_directory: None,
            permission_mode: Default::default(),
            created_at: crate::time::now(),
            updated_at: crate::time::now(),
        },
        messages: Vec::new(),
        events: Vec::new(),
        context: None,
        error: None,
    };
    let entry = state.make_entry(detail, 0).unwrap();
    let run = entry
        .runtime
        .start_run_with_files(&entry.agent, id, "Review", files)
        .await
        .unwrap();
    for event in run.events() {
        entry.snapshot.event(event).unwrap();
    }
    state.inner.runs.write().unwrap().insert(id.into(), entry);
}

#[tokio::test]
async fn durable_attachment_access_is_limited_to_task_family_and_survives_reopen() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let file = state
        .store_attachment("contract.pdf".into(), b"synthetic original".to_vec())
        .await
        .unwrap();
    let id = file.attachment.as_ref().unwrap().id.clone();
    record_run(&state, "owner", vec![file], None).await;
    record_run(&state, "child", vec![], Some("owner")).await;
    record_run(&state, "unrelated", vec![], None).await;
    for run in ["owner", "child"] {
        assert_eq!(
            state.attachment_bytes_for_run(run, &id).await.unwrap().1,
            b"synthetic original"
        );
        let (_, path) = state.attachment_path_for_run(run, &id).await.unwrap();
        assert_eq!(path, fs::canonicalize(path.clone()).unwrap());
    }
    assert!(
        state
            .attachment_bytes_for_run("unrelated", &id)
            .await
            .is_err()
    );
    assert!(
        state
            .attachment_bytes_for_run("missing", &id)
            .await
            .is_err()
    );
    assert!(state.message_file("owner", "event-0", 1).await.is_err());
    state.shutdown().await;
    drop(state);
    let state = AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        state
            .attachment_bytes_for_run("owner", &id)
            .await
            .unwrap()
            .1,
        b"synthetic original"
    );
    state.shutdown().await;
}

#[tokio::test]
async fn admission_rejects_unstored_references_before_starting_run() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let result = state
        .create(CreateRunInput {
            execution_policy: Default::default(),
            prompt: "Review".into(),
            files: vec![FileReference {
                name: "not-uploaded.txt".into(),
                path: None,
                text: String::new(),
                image: None,
                attachment: Some(AttachmentReference {
                    id: "a".repeat(64),
                    size: 10,
                }),
            }],
            model_id: MODEL_ID.into(),
            reasoning_effort: None,
            workspace_path: None,
            permission_mode: Default::default(),
        })
        .await;
    assert!(matches!(result, Err(ServiceError::BadRequest(_))));
    assert!(state.inner.runs.read().unwrap().is_empty());
    state.shutdown().await;
}
