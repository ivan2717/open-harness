use super::*;
use crate::contracts::Delegation;
use agent_core::agent::RunEvent;
use agent_runtime::{ModelResponse, PendingUserInput, ToolCall};

struct Fixture {
    directory: PathBuf,
    state: AppState,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-startup-recovery-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(directory.join("project")).unwrap();
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        Self { directory, state }
    }

    async fn save(&self, id: &str, owner: Option<(&str, &str)>) -> Arc<RunEntry> {
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                id: id.into(),
                revision: 0,
                title: id.into(),
                prompt: "startup recovery fixture".into(),
                status: RunStatus::Running,
                // No model is configured or invoked by these recovery tests.
                model_id: "recovery-fixture".into(),
                reasoning_effort: None,
                reasoning_settings_version: 1,
                model_history_through: None,
                workspace_path: Some(self.directory.join("project").display().to_string()),
                execution_directory: None,
                permission_mode: agent_local::PermissionMode::ReadOnly,
                execution_policy: Default::default(),
                compacting: false,
                created_at: now.clone(),
                updated_at: now,
                delegation: owner.map(|(parent, root)| Delegation {
                    parent_run_id: parent.into(),
                    root_run_id: root.into(),
                    task_name: id.into(),
                    write_paths: None,
                    model_override: None,
                    spawn_call_id: format!("spawn-{id}"),
                }),
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
            pending_inputs: Vec::new(),
            pending_inputs_paused: false,
        };
        let entry = self.state.make_entry(detail, 0).unwrap();
        entry
            .runtime
            .start_run(&entry.agent, id, "startup recovery fixture")
            .await
            .unwrap();
        self.state
            .inner
            .runs
            .write()
            .unwrap()
            .insert(id.into(), entry.clone());
        entry
    }

    fn snapshot(&self, id: &str) -> PathBuf {
        self.directory.join("runs").join(format!("{id}.json"))
    }

    fn journal(&self, id: &str) -> PathBuf {
        let hex: String = id.bytes().map(|byte| format!("{byte:02x}")).collect();
        self.directory
            .join("events")
            .join(format!("run-{hex}.jsonl"))
    }

    async fn reopen(&self) -> AppState {
        AppState::open_with_delay(self.directory.clone(), None, Duration::ZERO)
            .await
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn issue(state: &AppState, id: &str) -> RecoveryIssue {
    state
        .recovery_issues()
        .into_iter()
        .find(|issue| issue.run_id.as_deref() == Some(id))
        .unwrap()
}

#[tokio::test]
async fn recovery_diagnostics_do_not_echo_private_corrupt_values() {
    // Callsite interest is cached process-wide. A thread-local subscriber can
    // miss this event when another recovery test first registers it without a
    // subscriber. Match production's global subscriber in an isolated process,
    // so parallel tests cannot suppress events or contribute unrelated logs.
    const CAPTURE_PROCESS: &str = "OPENHARNESS_RECOVERY_CAPTURE_TEST";
    let test_name = std::thread::current()
        .name()
        .expect("the test runner names its test thread")
        .to_owned();
    if std::env::var(CAPTURE_PROCESS).as_deref() != Ok(test_name.as_str()) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--nocapture"])
            .env(CAPTURE_PROCESS, &test_name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated recovery diagnostics failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("RECOVERY_CAPTURE_VERIFIED"),
            "the isolated process must execute the complete diagnostic assertions"
        );
        return;
    }

    #[derive(Clone)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let fixture = Fixture::new().await;
    fixture.save("healthy", None).await;
    fixture.save("bad-journal", None).await;
    fixture.save("bad-owner", None).await;
    // A malformed enum/numeric value can appear verbatim in serde's error.
    let secret = "sk-private-fixture-value-never-print";
    let journal = format!("{{\"schema_version\":\"{secret}\"}}\n");
    fs::write(fixture.journal("bad-journal"), journal.as_bytes()).unwrap();
    let mut snapshot: RunDetail =
        serde_json::from_slice(&fs::read(fixture.snapshot("bad-owner")).unwrap()).unwrap();
    snapshot.summary.delegation = Some(Delegation {
        parent_run_id: secret.into(),
        root_run_id: secret.into(),
        task_name: "test".into(),
        write_paths: None,
        model_override: None,
        spawn_call_id: "spawn-test".into(),
    });
    fs::write(
        fixture.snapshot("bad-owner"),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    assert!(
        valid_id(secret),
        "valid syntax alone cannot redact private values"
    );

    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = Capture(captured.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let restored = fixture.reopen().await;
    let diagnostics = serde_json::to_string(&restored.recovery_issues()).unwrap();
    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("conversation recovery skipped"));
    assert!(!logs.contains(secret));
    assert!(!diagnostics.contains(secret));
    assert_eq!(issue(&restored, "bad-owner").related_run_id, None);
    assert_eq!(restored.list().len(), 1);
    assert_eq!(
        fs::read(fixture.journal("bad-journal")).unwrap(),
        journal.as_bytes()
    );
    println!("RECOVERY_CAPTURE_VERIFIED");
}

#[tokio::test]
async fn corrupt_snapshot_and_committed_journal_are_isolated_and_preserved() {
    let fixture = Fixture::new().await;
    for id in ["healthy", "bad-snapshot", "bad-journal", "missing-journal"] {
        fixture.save(id, None).await;
    }
    let snapshot_evidence = b"{broken snapshot";
    let journal_evidence = b"{broken committed record}\n";
    fs::write(fixture.snapshot("bad-snapshot"), snapshot_evidence).unwrap();
    fs::write(fixture.journal("bad-journal"), journal_evidence).unwrap();
    fs::remove_file(fixture.journal("missing-journal")).unwrap();

    for _ in 0..2 {
        let restored = fixture.reopen().await;
        assert_eq!(
            restored.get("healthy").unwrap().summary.status,
            RunStatus::Interrupted
        );
        assert_eq!(restored.list().len(), 1);
        assert_eq!(restored.recovery_issues().len(), 3);
        assert_eq!(
            issue(&restored, "bad-snapshot").stage,
            RecoveryStage::Snapshot
        );
        assert_eq!(
            issue(&restored, "bad-journal").stage,
            RecoveryStage::EventLog
        );
        assert!(issue(&restored, "missing-journal").message.contains("缺少"));
        // Diagnostic values are serializable but are not repairs or deletions.
        assert!(
            serde_json::to_value(restored.recovery_issues())
                .unwrap()
                .is_array()
        );
        assert_eq!(
            fs::read(fixture.snapshot("bad-snapshot")).unwrap(),
            snapshot_evidence
        );
        assert_eq!(
            fs::read(fixture.journal("bad-journal")).unwrap(),
            journal_evidence
        );
        assert!(fixture.snapshot("missing-journal").is_file());
    }
}

#[tokio::test]
async fn damaged_root_or_intermediate_parent_cannot_release_stale_child_permissions() {
    for damaged in ["z-root", "y-parent"] {
        let fixture = Fixture::new().await;
        fixture.save("healthy", None).await;
        fixture.save("z-root", None).await;
        fixture.save("y-parent", Some(("z-root", "z-root"))).await;
        let child = fixture.save("a-child", Some(("y-parent", "z-root"))).await;
        child
            .snapshot
            .update(|detail| {
                detail.summary.permission_mode = agent_local::PermissionMode::WorkspaceWrite;
                detail.summary.execution_policy.approval_mode =
                    agent_local::ApprovalMode::FullAccess;
            })
            .unwrap();
        let child_evidence = fs::read(fixture.snapshot("a-child")).unwrap();
        fs::write(fixture.snapshot(damaged), b"broken authority").unwrap();

        let restored = fixture.reopen().await;
        assert!(restored.get("healthy").is_ok());
        assert!(restored.get("a-child").is_err());
        assert_eq!(issue(&restored, "a-child").stage, RecoveryStage::Dependency);
        assert_eq!(
            issue(&restored, "a-child").related_run_id.as_deref(),
            Some(damaged)
        );
        assert_eq!(
            fs::read(fixture.snapshot("a-child")).unwrap(),
            child_evidence
        );
        assert!(restored.resume("a-child", None).await.is_err());
        if damaged == "z-root" {
            assert!(restored.get("y-parent").is_err());
        }
    }
}

#[tokio::test]
async fn valid_parent_chain_restores_in_dependency_order_and_inherits_root_permissions() {
    let fixture = Fixture::new().await;
    fixture.save("z-root", None).await;
    fixture.save("y-parent", Some(("z-root", "z-root"))).await;
    let child = fixture.save("a-child", Some(("y-parent", "z-root"))).await;
    child
        .snapshot
        .update(|detail| {
            detail.summary.permission_mode = agent_local::PermissionMode::WorkspaceWrite;
            detail.summary.execution_policy.approval_mode = agent_local::ApprovalMode::FullAccess;
        })
        .unwrap();

    let restored = fixture.reopen().await;
    assert!(restored.recovery_issues().is_empty());
    for id in ["z-root", "y-parent", "a-child"] {
        let entry = restored.entry(id).unwrap();
        let summary = entry.snapshot.get().summary;
        assert_eq!(
            summary.permission_mode,
            agent_local::PermissionMode::ReadOnly
        );
        assert_eq!(
            summary.execution_policy.approval_mode,
            agent_local::ApprovalMode::Ask
        );
        assert!(!entry.workspace.as_ref().unwrap().full_access());
    }
}

#[tokio::test]
async fn inconsistent_or_cyclic_parentage_is_not_restored_as_an_independent_task() {
    let fixture = Fixture::new().await;
    fixture.save("root-one", None).await;
    fixture.save("root-two", None).await;
    fixture
        .save("foreign-child", Some(("root-two", "root-one")))
        .await;
    fixture
        .save("cycle-one", Some(("cycle-two", "root-one")))
        .await;
    fixture
        .save("cycle-two", Some(("cycle-one", "root-one")))
        .await;

    let restored = fixture.reopen().await;
    assert_eq!(restored.list().len(), 2);
    for id in ["foreign-child", "cycle-one", "cycle-two"] {
        assert!(restored.entry(id).is_err());
        assert_eq!(issue(&restored, id).stage, RecoveryStage::Dependency);
    }
}

#[tokio::test]
async fn completion_settlement_failure_does_not_block_healthy_roots_or_restore_descendants() {
    let fixture = Fixture::new().await;
    fixture.save("root", None).await;
    fixture.save("healthy", None).await;
    fixture.save("child", Some(("root", "root"))).await;
    fixture.save("descendant", Some(("child", "root"))).await;
    // The mailbox can be read, but committing completion cannot open its temp file.
    fs::create_dir(fixture.directory.join("collaboration.json.tmp")).unwrap();
    let before = fs::read(fixture.journal("child")).unwrap();

    let restored = fixture.reopen().await;
    assert!(restored.get("root").is_ok());
    assert!(restored.get("healthy").is_ok());
    assert!(restored.get("child").is_err());
    assert_eq!(issue(&restored, "child").stage, RecoveryStage::Settlement);
    assert_eq!(
        issue(&restored, "descendant").stage,
        RecoveryStage::Dependency
    );
    assert_eq!(fs::read(fixture.journal("child")).unwrap(), before);
}

#[tokio::test]
async fn restart_keeps_pending_commands_and_steering_idle_without_replaying_side_effects() {
    let fixture = Fixture::new().await;
    let entry = fixture.save("active", None).await;
    let mut run = entry.runtime.restore_run("active").await.unwrap().unwrap();
    run.begin_step().unwrap();
    let call = ToolCall {
        id: "pending-write".into(),
        name: "write_file".into(),
        arguments: r#"{"path":"must-not-exist.txt","content":"not authorized by restart","expected_content":null}"#.into(),
    };
    entry
        .runtime
        .record_event(
            &mut run,
            RunEvent::ModelResponseReceived {
                content: "write the file".into(),
                response: Some(ModelResponse::tool_calls(
                    "write the file".into(),
                    vec![call.clone()],
                )),
                provider_state: None,
                provider_continuation: None,
                token_usage: None,
            },
        )
        .await
        .unwrap();
    entry
        .runtime
        .record_event(
            &mut run,
            RunEvent::ToolCallRequested {
                call_id: call.id,
                tool_name: call.name,
                arguments: call.arguments,
            },
        )
        .await
        .unwrap();
    entry
        .snapshot
        .update(|detail| {
            detail.pending_inputs.push(PendingUserInput {
                id: "queued-draft".into(),
                content: "continue and write another file".into(),
                files: Vec::new(),
                steer: true,
                delivering: true,
                revision: 1,
            });
        })
        .unwrap();
    let before = fs::read(fixture.journal("active")).unwrap();

    let restored = fixture.reopen().await;
    tokio::task::yield_now().await;
    let entry = restored.entry("active").unwrap();
    let detail = entry.snapshot.get();
    assert_eq!(detail.summary.status, RunStatus::Interrupted);
    assert!(detail.pending_inputs_paused);
    assert!(!detail.pending_inputs[0].steer);
    assert!(!detail.pending_inputs[0].delivering);
    assert!(!entry.runtime.is_executing("active"));
    assert!(!entry.background.load(Ordering::Acquire));
    assert_eq!(fs::read(fixture.journal("active")).unwrap(), before);
    assert!(
        !fixture
            .directory
            .join("project/must-not-exist.txt")
            .exists()
    );
}

#[tokio::test]
async fn invalid_storage_directory_is_a_global_error_not_a_list_of_bad_conversations() {
    let fixture = Fixture::new().await;
    fs::write(fixture.directory.join("events"), b"not a directory").unwrap();
    assert!(
        AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inaccessible_journal_directory_remains_a_global_error() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await;
    fixture.save("healthy", None).await;
    let directory = fixture.directory.join("events");
    let permissions = fs::metadata(&directory).unwrap().permissions();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o0)).unwrap();
    let denied = fs::read_dir(&directory).is_err();
    let reopened = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO).await;
    fs::set_permissions(&directory, permissions).unwrap();
    // Privileged test runners may legitimately bypass directory permissions.
    if denied {
        assert!(reopened.is_err());
    }
}
