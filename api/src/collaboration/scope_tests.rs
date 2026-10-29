use super::*;
use agent_local::PermissionMode;
use agent_providers::ProviderProtocol;
use agent_runtime::ToolExecutor;

struct Fixture {
    state: AppState,
    directory: PathBuf,
    project: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-scope-recovery-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        let state =
            AppState::open_with_delay(directory.join("state"), None, Duration::from_millis(30))
                .await
                .unwrap();
        Self {
            state,
            directory,
            project,
        }
    }

    async fn configure(&self, name: &str) -> String {
        self.state
            .configure_model(ConfigureModel {
                protocol: ProviderProtocol::ChatCompletions,
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: "local-test-only".into(),
                model: name.into(),
                context_window: 128_000,
                max_output_tokens: 4096,
                reasoning_mode: "off".into(),
                reasoning_effort: "high".into(),
                reasoning_capability: None,
                reasoning_default: None,
                supports_images: false,
            })
            .await
            .unwrap()
            .id
    }

    async fn member(
        &self,
        id: &str,
        model: &str,
        mode: PermissionMode,
        paths: Option<Vec<String>>,
    ) -> Arc<RunEntry> {
        let child = id != "root";
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                delegation: child.then(|| Delegation {
                    parent_run_id: "root".into(),
                    root_run_id: "root".into(),
                    task_name: "worker".into(),
                    write_paths: paths,
                    model_override: None,
                    spawn_call_id: "spawn-worker".into(),
                }),
                execution_policy: Default::default(),
                id: id.into(),
                revision: 0,
                title: id.into(),
                prompt: "scope recovery".into(),
                status: RunStatus::Running,
                compacting: false,
                model_id: model.into(),
                reasoning_effort: None,
                reasoning_settings_version: 1,
                model_history_through: None,
                workspace_path: Some(self.project.display().to_string()),
                execution_directory: None,
                permission_mode: mode,
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let entry = self.state.make_entry(detail, 0).unwrap();
        assert!(
            entry.workspace_error.is_none(),
            "{:?}",
            entry.workspace_error
        );
        let run = entry
            .runtime
            .start_run(&entry.agent, id, "scope recovery")
            .await
            .unwrap();
        for event in run.events() {
            entry.snapshot.event(event).unwrap();
        }
        if child {
            cancel_restored(&entry, run).await.unwrap();
        }
        self.state
            .inner
            .runs
            .write()
            .unwrap()
            .insert(id.into(), entry.clone());
        entry
    }

    async fn stop_child(&self) {
        self.state.cancel_member("child", false).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while AppState::active(&self.state.entry("child").unwrap()) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn paths(path: &str) -> Option<Vec<String>> {
    Some(vec![path.into()])
}
fn write(path: &str) -> ToolCall {
    ToolCall {
        id: "write".into(),
        name: "write_file".into(),
        arguments: json!({"path":path,"content":"verified","expected_content":null}).to_string(),
    }
}

#[tokio::test]
async fn restoring_stale_delegate_permissions_cannot_override_the_root_policy() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::ReadOnly, None)
        .await;
    let child = f
        .member("child", MODEL_ID, PermissionMode::ReadOnly, None)
        .await;
    // Simulate interruption after the root's downgrade was saved, before
    // older delegate snapshots were updated.
    child
        .snapshot
        .update(|detail| {
            detail.summary.execution_policy.approval_mode = agent_local::ApprovalMode::FullAccess;
            detail.summary.permission_mode = PermissionMode::WorkspaceWrite;
        })
        .unwrap();
    f.state.shutdown().await;
    let restored = AppState::open_with_delay(f.directory.join("state"), None, Duration::ZERO)
        .await
        .unwrap();
    for id in ["root", "child"] {
        let entry = restored.entry(id).unwrap();
        assert_eq!(
            entry.snapshot.get().summary.execution_policy.approval_mode,
            agent_local::ApprovalMode::Ask
        );
        assert_eq!(
            entry.snapshot.get().summary.permission_mode,
            PermissionMode::ReadOnly
        );
        assert_eq!(
            entry.workspace.as_ref().unwrap().approval_mode().unwrap(),
            agent_local::ApprovalMode::Ask
        );
        assert!(!entry.workspace.as_ref().unwrap().full_access());
        assert!(
            entry
                .workspace
                .as_ref()
                .unwrap()
                .execute(write("forbidden.txt"))
                .await
                .is_err()
        );
    }
    restored.shutdown().await;
}

#[tokio::test]
async fn rebuilding_a_child_preserves_full_root_policy_and_child_write_restrictions() {
    let f = Fixture::new().await;
    let root = f
        .member("root", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    root.snapshot
        .update(|detail| {
            detail.summary.execution_policy.approval_mode = agent_local::ApprovalMode::FullAccess;
        })
        .unwrap();
    root.workspace
        .as_ref()
        .unwrap()
        .set_approval_mode(agent_local::ApprovalMode::FullAccess)
        .unwrap();
    // Fixture child input defaults to Ask; only the root is authoritative.
    let child = f
        .member("child", MODEL_ID, PermissionMode::ReadOnly, Some(vec![]))
        .await;
    assert_eq!(
        child.snapshot.get().summary.execution_policy.approval_mode,
        agent_local::ApprovalMode::FullAccess
    );
    assert_eq!(
        root.workspace.as_ref().unwrap().approval_mode().unwrap(),
        agent_local::ApprovalMode::FullAccess
    );
    assert!(root.workspace.as_ref().unwrap().full_access());
    assert!(!child.workspace.as_ref().unwrap().full_access());
    assert_eq!(
        child.snapshot.get().summary.permission_mode,
        PermissionMode::ReadOnly
    );
    assert!(
        child
            .workspace
            .as_ref()
            .unwrap()
            .execute(write("forbidden.txt"))
            .await
            .is_err()
    );
    f.state.shutdown().await;
}

#[tokio::test]
async fn followup_rebinds_a_stale_delegate_approval_mode_to_the_live_root() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    let child = f
        .member("child", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    child
        .snapshot
        .update(|detail| {
            detail.summary.execution_policy.approval_mode = agent_local::ApprovalMode::FullAccess;
        })
        .unwrap();
    f.state
        .resume_child("root", "child", "Continue reviewing".into(), None)
        .await
        .unwrap();
    let resumed = f.state.entry("child").unwrap();
    assert!(!Arc::ptr_eq(&child, &resumed));
    assert_eq!(
        resumed
            .snapshot
            .get()
            .summary
            .execution_policy
            .approval_mode,
        agent_local::ApprovalMode::Ask
    );
    assert_eq!(
        resumed.workspace.as_ref().unwrap().approval_mode().unwrap(),
        agent_local::ApprovalMode::Ask
    );
    assert!(!resumed.workspace.as_ref().unwrap().full_access());
    f.stop_child().await;
    f.state.shutdown().await;
}

#[tokio::test]
async fn new_task_names_cannot_shadow_identity_aliases_or_paths() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    for name in [
        "self",
        "parent",
        "root",
        "reader/check",
        " reader",
        "reader ",
    ] {
        let error = f
            .state
            .spawn_child(
                "root",
                "spawn",
                SpawnInput {
                    task_name: name.into(),
                    task: "Inspect a file".into(),
                    write_paths: None,
                    model: None,
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("self、parent、root"));
        assert_eq!(f.state.members("root").len(), 1);
    }
    f.state.shutdown().await;
}

#[tokio::test]
async fn stopped_child_can_correct_scope_and_reset_to_parent_without_losing_identity_or_history() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    let old = f
        .member(
            "child",
            MODEL_ID,
            PermissionMode::WorkspaceWrite,
            paths(".openharness-tmp"),
        )
        .await;
    let history = old.snapshot.get().events;
    f.state
        .resume_child(
            "root",
            "child",
            "Write the script in the corrected file".into(),
            Some(paths(".openharness-tmp/reconcile.js")),
        )
        .await
        .unwrap();
    let current = f.state.entry("child").unwrap();
    assert!(!Arc::ptr_eq(&old, &current));
    assert_eq!(current.snapshot.get().summary.id, "child");
    assert_eq!(
        current
            .snapshot
            .get()
            .summary
            .delegation
            .unwrap()
            .write_paths,
        paths(".openharness-tmp/reconcile.js")
    );
    assert!(current.snapshot.get().events.len() > history.len());
    assert_eq!(
        serde_json::to_value(&current.snapshot.get().events[..history.len()]).unwrap(),
        serde_json::to_value(&history).unwrap(),
    );
    current
        .workspace
        .as_ref()
        .unwrap()
        .execute(write(".openharness-tmp/reconcile.js"))
        .await
        .unwrap();
    assert!(
        current
            .workspace
            .as_ref()
            .unwrap()
            .execute(write("outside.js"))
            .await
            .is_err()
    );
    f.stop_child().await;

    f.state
        .resume_child(
            "root",
            "child",
            "Use inherited workspace permission".into(),
            Some(None),
        )
        .await
        .unwrap();
    let current = f.state.entry("child").unwrap();
    assert_eq!(
        current
            .snapshot
            .get()
            .summary
            .delegation
            .unwrap()
            .write_paths,
        None
    );
    assert_eq!(
        current.snapshot.get().summary.permission_mode,
        PermissionMode::WorkspaceWrite
    );
    current
        .workspace
        .as_ref()
        .unwrap()
        .execute(write("outside.js"))
        .await
        .unwrap();
    f.stop_child().await;

    f.state
        .resume_child(
            "root",
            "child",
            "Review only".into(),
            Some(Some(Vec::new())),
        )
        .await
        .unwrap();
    let current = f.state.entry("child").unwrap();
    assert_eq!(
        current.snapshot.get().summary.permission_mode,
        PermissionMode::ReadOnly
    );
    assert_eq!(
        current
            .snapshot
            .get()
            .summary
            .delegation
            .unwrap()
            .write_paths,
        Some(Vec::new())
    );
    assert!(
        current
            .workspace
            .as_ref()
            .unwrap()
            .execute(write("denied.js"))
            .await
            .is_err()
    );
    f.state.shutdown().await;
}

#[tokio::test]
async fn failed_scope_change_keeps_previous_model_scope_and_history_and_reserves_nothing() {
    let f = Fixture::new().await;
    let old_model = f.configure("old-model").await;
    let next_model = f.configure("new-model").await;
    f.member("root", &next_model, PermissionMode::WorkspaceWrite, None)
        .await;
    let old = f
        .member(
            "child",
            &old_model,
            PermissionMode::WorkspaceWrite,
            paths("old.js"),
        )
        .await;
    let before = old.snapshot.get();
    let access = f
        .state
        .workspace_access(f.project.to_str().unwrap())
        .unwrap();
    access.try_acquire("peer", &["busy.js".into()]).unwrap();
    assert!(
        f.state
            .resume_child(
                "root",
                "child",
                "Correct scope".into(),
                Some(Some(vec!["free.js".into(), "busy.js".into()])),
            )
            .await
            .is_err()
    );
    let current = f.state.entry("child").unwrap();
    assert!(Arc::ptr_eq(&old, &current));
    let after = current.snapshot.get();
    assert_eq!(after.summary.model_id, old_model);
    assert_eq!(
        after.summary.delegation.unwrap().write_paths,
        paths("old.js")
    );
    assert_eq!(after.summary.revision, before.summary.revision);
    assert_eq!(after.events.len(), before.events.len());
    access.try_acquire("another", &["free.js".into()]).unwrap();
    access.release("peer").await;
    access.release("another").await;
    f.state.shutdown().await;
}

#[tokio::test]
async fn failed_durable_resume_never_publishes_new_scope_or_model_and_releases_reservation() {
    let f = Fixture::new().await;
    let old_model = f.configure("old-model").await;
    let next_model = f.configure("new-model").await;
    f.member("root", &next_model, PermissionMode::WorkspaceWrite, None)
        .await;
    let old = f
        .member(
            "child",
            &old_model,
            PermissionMode::WorkspaceWrite,
            paths("old.js"),
        )
        .await;
    let blocked_snapshot = f.directory.join("state/runs/child.json.tmp");
    fs::create_dir(&blocked_snapshot).unwrap();

    assert!(
        f.state
            .resume_child(
                "root",
                "child",
                "Resume with corrected scope".into(),
                Some(paths("corrected.js")),
            )
            .await
            .is_err()
    );
    let current = f.state.entry("child").unwrap();
    assert!(Arc::ptr_eq(&old, &current));
    assert!(!current.background.load(Ordering::Acquire));
    assert_eq!(current.snapshot.get().summary.model_id, old_model);
    assert_eq!(
        current
            .snapshot
            .get()
            .summary
            .delegation
            .unwrap()
            .write_paths,
        paths("old.js")
    );
    let durable: RunDetail =
        serde_json::from_slice(&fs::read(f.directory.join("state/runs/child.json")).unwrap())
            .unwrap();
    assert_eq!(durable.summary.model_id, old_model);
    assert_eq!(
        durable.summary.delegation.unwrap().write_paths,
        paths("old.js")
    );
    let access = f
        .state
        .workspace_access(f.project.to_str().unwrap())
        .unwrap();
    access
        .try_acquire("other", &["corrected.js".into()])
        .unwrap();
    access.release("other").await;

    fs::remove_dir(blocked_snapshot).unwrap();
    // Resume input may have reached the authoritative log before the snapshot
    // failed. It must remain unexecuted and recoverable, never auto-replayed.
    f.state.cancel_member("child", false).await.unwrap();
    f.state.shutdown().await;
}

#[tokio::test]
async fn inherited_scope_never_exceeds_parent_permission_and_live_scope_cannot_change() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::ReadOnly, None)
        .await;
    f.member(
        "child",
        MODEL_ID,
        PermissionMode::ReadOnly,
        Some(Vec::new()),
    )
    .await;
    assert!(
        f.state
            .resume_child("root", "child", "Try writing".into(), Some(paths("bad.js")))
            .await
            .is_err()
    );
    f.state
        .resume_child("root", "child", "Inherit read-only".into(), Some(None))
        .await
        .unwrap();
    let current = f.state.entry("child").unwrap();
    assert_eq!(
        current.snapshot.get().summary.permission_mode,
        PermissionMode::ReadOnly
    );
    assert!(
        current
            .workspace
            .as_ref()
            .unwrap()
            .execute(write("bad.js"))
            .await
            .is_err()
    );
    assert!(
        f.state
            .resume_child(
                "root",
                "child",
                "Change while running".into(),
                Some(Some(Vec::new())),
            )
            .await
            .is_err()
    );
    assert!(
        f.state
            .resume_child("root", "child", "A steering message".into(), None)
            .await
            .is_err()
    );
    assert!(
        !f.state
            .inner
            .mailboxes
            .pending("child")
            .unwrap()
            .iter()
            .any(|m| m.text == "A steering message")
    );
    let result: serde_json::Value = serde_json::from_str(
        &f.state
            .control_agent(
                "root",
                serde_json::from_value(
                    json!({"action":"send","target":"worker","message":"A steering message"}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["accepted"], true);
    assert_eq!(result["starts_idle_agent"], false);
    f.state.shutdown().await;
}

#[tokio::test]
async fn followup_rejects_terminal_cleanup_without_queueing_then_starts_once_idle() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    let child = f
        .member("child", MODEL_ID, PermissionMode::WorkspaceWrite, None)
        .await;
    // Terminal state is observable before the wrapper has finished releasing
    // resources. A followup must not return success for an unprocessed inbox
    // message in this window, even though no further model step will run.
    child.background.store(true, Ordering::Release);
    let before = child.snapshot.get().events.len();
    let error = f
        .state
        .resume_child("root", "child", "Verify the script".into(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("followup 只能继续空闲子任务"));
    assert!(f.state.inner.mailboxes.pending("child").unwrap().is_empty());
    assert_eq!(child.snapshot.get().events.len(), before);

    child.background.store(false, Ordering::Release);
    f.state
        .resume_child("root", "child", "Verify the script".into(), None)
        .await
        .unwrap();
    assert!(AppState::active(&f.state.entry("child").unwrap()));
    let run = child.runtime.restore_run("child").await.unwrap().unwrap();
    assert_eq!(
        run.events()
            .iter()
            .filter(|event| {
                matches!(&event.event, agent_core::agent::RunEvent::RunResumed { .. })
            })
            .count(),
        1
    );
    f.state.shutdown().await;
}

#[tokio::test]
async fn followup_message_limit_is_bytes_and_rejection_has_no_side_effects() {
    let f = Fixture::new().await;
    f.member("root", MODEL_ID, PermissionMode::ReadOnly, None)
        .await;
    let child = f
        .member("child", MODEL_ID, PermissionMode::ReadOnly, None)
        .await;
    let before = child.snapshot.get();
    for message in [" \n ".into(), "x".repeat(16 * 1024 + 1), "续".repeat(6000)] {
        let error = f
            .state
            .resume_child("root", "child", message, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("16384 bytes"));
        assert_eq!(
            child.snapshot.get().summary.revision,
            before.summary.revision
        );
        assert_eq!(child.snapshot.get().events.len(), before.events.len());
        assert!(f.state.inner.mailboxes.pending("child").unwrap().is_empty());
        assert!(!AppState::active(&child));
    }
    f.state
        .resume_child("root", "child", "x".repeat(16 * 1024), None)
        .await
        .unwrap();
    assert!(AppState::active(&f.state.entry("child").unwrap()));
    f.state.shutdown().await;
}
