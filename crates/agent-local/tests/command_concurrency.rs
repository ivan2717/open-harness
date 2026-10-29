//! These exercise the actual macOS sandbox, not a shell-command allowlist.
#![cfg(target_os = "macos")]

use agent_local::{
    CommandAccess, CommandApprovalScope, CommandStatus, LocalTools, NetworkAccess, PendingApproval,
    PermissionMode, WorkspaceAccess,
};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::task::JoinHandle;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    access: WorkspaceAccess,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "openharness-command-concurrency-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let access = WorkspaceAccess::new(root.clone()).unwrap();
        Self { base, root, access }
    }

    fn tools(&self, owner: &str, child: bool) -> LocalTools {
        LocalTools::open(
            self.root.clone(),
            self.base.join(owner),
            if child {
                PermissionMode::ReadOnly
            } else {
                PermissionMode::WorkspaceWrite
            },
        )
        .unwrap()
        .with_collaboration(self.access.clone(), owner, child.then(Vec::new))
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "concurrency-test".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

fn submit(tools: &LocalTools, arguments: Value) -> JoinHandle<io::Result<String>> {
    let tools = tools.clone();
    tokio::spawn(async move { tools.execute(call("run_command", arguments)).await })
}

async fn pending(tools: &LocalTools) -> PendingApproval {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = tools.review().unwrap().approvals.into_iter().next() {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("command must explicitly request approval")
}

async fn result(task: JoinHandle<io::Result<String>>) -> CommandStatus {
    serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("command did not return")
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

async fn completed(tools: &LocalTools, mut state: CommandStatus) -> CommandStatus {
    tokio::time::timeout(Duration::from_secs(8), async {
        while state.status == "running" {
            state = tools
                .command_status(&state.command_id, 0, 100)
                .await
                .unwrap();
            if state.status == "running" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        state
    })
    .await
    .expect("command did not finish")
}

async fn deny_requested(tools: &LocalTools, mut arguments: Value) {
    arguments["network_access"] = "allowed".into();
    let task = submit(tools, arguments);
    let approval = pending(tools).await;
    tools.approve(&approval.id, false).unwrap();
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn inherited_write_commands_share_root_approval_and_obey_leases_locks_and_revocation() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", false);
    // No file scope narrows this child's inherited workspace-write permission.
    let child = fixture
        .tools("inheriting-child", false)
        .with_command_approval_scope("parent")
        .unwrap();
    let first = submit(
        &parent,
        json!({"command":"printf parent > parent.txt", "network_access":"allowed"}),
    );
    let approval = pending(&parent).await;
    assert_eq!(approval.access, CommandAccess::WorkspaceWrite);
    parent
        .approve_with_scope(&approval.id, true, CommandApprovalScope::Session)
        .unwrap();
    assert_eq!(
        completed(&parent, result(first).await).await.status,
        "completed"
    );

    let written = completed(
        &child,
        result(submit(
            &child,
            json!({"command":"printf child > child.txt", "network_access":"allowed"}),
        ))
        .await,
    )
    .await;
    assert_eq!(written.status, "completed", "{}", written.output);
    assert_eq!(
        fs::read_to_string(fixture.root.join("child.txt")).unwrap(),
        "child"
    );
    assert!(child.review().unwrap().approvals.is_empty());
    assert!(
        child
            .review()
            .unwrap()
            .changes
            .iter()
            .any(|change| change.path == "child.txt")
    );

    fixture
        .access
        .acquire("scoped-peer", &["reserved.txt".into()])
        .await
        .unwrap();
    let error = child
        .execute(call(
            "run_command",
            json!({
                "command":"printf invalid > reserved.txt", "access":"workspace_write"
            }),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!fixture.root.join("reserved.txt").exists());
    fixture.access.release("scoped-peer").await;

    let running = result(submit(
        &child,
        json!({
            "command":"printf RUNNING; sleep 20", "yield_time_ms":0, "timeout_seconds":25
        }),
    ))
    .await;
    assert_eq!(running.status, "running");
    let error = parent
        .execute(call(
            "run_command",
            json!({
                "command":"printf invalid > raced.txt", "access":"workspace_write"
            }),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(!fixture.root.join("raced.txt").exists());
    assert_eq!(
        child
            .stop_command(&running.command_id)
            .await
            .unwrap()
            .status,
        "cancelled"
    );

    child.revoke_command_grants().unwrap();
    assert!(parent.review().unwrap().command_grants.is_empty());
    deny_requested(
        &parent,
        json!({"command":"printf invalid > denied-parent.txt"}),
    )
    .await;
    deny_requested(
        &child,
        json!({"command":"printf invalid > denied-child.txt"}),
    )
    .await;
    assert!(!fixture.root.join("denied-parent.txt").exists());
    assert!(!fixture.root.join("denied-child.txt").exists());
}

#[tokio::test]
async fn read_only_commands_and_delegated_tests_share_live_workspace_but_not_scratch() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("source.txt"), "before").unwrap();
    fs::write(fixture.root.join(".env"), "not-a-real-secret").unwrap();
    let parent = fixture.tools("parent", false);
    assert!(parent.review().unwrap().sandbox_available);
    let first = submit(
        &parent,
        json!({
            "command":"printf private > \"$TMPDIR/only-first\"; printf 'SCRATCH=%s\nREADY\n' \"$TMPDIR\"; sleep 20",
            "access":"read_only", "yield_time_ms":0, "timeout_seconds":25
        }),
    );
    assert!(parent.review().unwrap().approvals.is_empty());
    let first = result(first).await;
    assert_eq!(first.status, "running");
    let mut ready = first.clone();
    for _ in 0..100 {
        ready = parent
            .command_status(&first.command_id, 0, 100)
            .await
            .unwrap();
        if ready.output.contains("READY") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.output.contains("READY"), "{}", ready.output);
    let scratch = ready
        .output
        .lines()
        .find_map(|line| line.strip_prefix("SCRATCH="))
        .unwrap();
    assert!(!PathBuf::from(scratch).starts_with(fs::canonicalize(&fixture.root).unwrap()));
    assert!(!parent.review().unwrap().busy);
    assert!(!parent.review().unwrap().review_incomplete);

    // A running test neither owns the mutation lock nor snapshots the project.
    parent
        .execute(call(
            "write_file",
            json!({
                "path":"source.txt", "expected_content":"before", "content":"after"
            }),
        ))
        .await
        .unwrap();
    let child = fixture
        .tools("child", true)
        .with_command_approval_scope("parent")
        .unwrap();
    let script = format!(
        "if printf bad > source.txt 2>/dev/null; then exit 81; fi; \
         if cat .env >/dev/null 2>&1; then exit 82; fi; \
         if cat '{}/only-first' >/dev/null 2>&1; then exit 83; fi; \
         printf child > \"$TMPDIR/result.txt\" || exit 84; \
         test \"$(cat source.txt)\" = after || exit 85; \
         printf 'SECOND=%s\\n' \"$TMPDIR\"; printf TEST_OK",
        scratch.replace('\'', "'\\''")
    );
    // Omitting access is read-only for a child. It inherits only its root grant.
    let second = completed(
        &child,
        result(submit(
            &child,
            json!({
                "command":script, "yield_time_ms":0, "timeout_seconds":5
            }),
        ))
        .await,
    )
    .await;
    assert_eq!(second.status, "completed", "{}", second.output);
    assert!(second.output.contains("TEST_OK"));
    let second_scratch = second
        .output
        .lines()
        .find_map(|line| line.strip_prefix("SECOND="))
        .unwrap();
    assert_ne!(scratch, second_scratch);
    assert!(
        !PathBuf::from(second_scratch).exists(),
        "completed command still retains its scratch directory"
    );
    assert!(parent.commands_running());
    assert_eq!(
        fs::read_to_string(fixture.root.join("source.txt")).unwrap(),
        "after"
    );
    assert!(!child.review().unwrap().review_incomplete);
    assert!(child.review().unwrap().changes.is_empty());
    assert_eq!(
        child.review().unwrap().commands[0].access,
        CommandAccess::ReadOnly
    );
    // Revocation stops future reuse without interrupting already-approved work.
    child.revoke_command_grants().unwrap();
    assert!(parent.review().unwrap().command_grants.is_empty());
    assert_eq!(
        parent
            .command_status(&first.command_id, 0, 0)
            .await
            .unwrap()
            .status,
        "running"
    );
    assert_eq!(
        parent.stop_command(&first.command_id).await.unwrap().status,
        "cancelled"
    );
    assert!(
        !PathBuf::from(scratch).exists(),
        "stopped command still retains its scratch directory"
    );
}

#[tokio::test]
async fn reusable_approval_is_explicit_scoped_revocable_and_cannot_escalate_a_child() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", false);
    let input = json!({"command":"printf OK", "access":"read_only", "network_access":"allowed", "timeout_seconds":5});
    let first = submit(&parent, input.clone());
    let approval = pending(&parent).await;
    parent.approve(&approval.id, true).unwrap();
    assert_eq!(result(first).await.status, "completed");
    assert!(parent.review().unwrap().command_grants.is_empty());
    let second = submit(&parent, input.clone());
    let approval = pending(&parent).await;
    parent
        .approve_with_scope(&approval.id, true, CommandApprovalScope::Session)
        .unwrap();
    assert_eq!(result(second).await.status, "completed");
    assert_eq!(
        result(submit(&parent, input.clone())).await.status,
        "completed"
    );
    assert!(parent.review().unwrap().approvals.is_empty());
    assert_eq!(
        parent.review().unwrap().command_grants[0].network_access,
        NetworkAccess::Allowed
    );
    deny_requested(
        &parent,
        json!({"command":"printf OK", "access":"workspace_write", "network_access":"allowed"}),
    )
    .await;
    let unrelated = fixture.tools("unrelated", false);
    deny_requested(&unrelated, input.clone()).await;
    // Even a broad root grant never changes the child's filesystem ceiling.
    let broad = submit(
        &parent,
        json!({"command":"printf OK", "access":"workspace_write", "network_access":"allowed"}),
    );
    let approval = pending(&parent).await;
    parent
        .approve_with_scope(&approval.id, true, CommandApprovalScope::Session)
        .unwrap();
    assert_eq!(result(broad).await.status, "completed");
    let child = fixture
        .tools("child", true)
        .with_command_approval_scope("parent")
        .unwrap();
    assert_eq!(
        result(submit(&child, json!({"command":"printf CHILD_OK"})))
            .await
            .output,
        "CHILD_OK"
    );
    let rejected = child
        .execute(call(
            "run_command",
            json!({
                "command":"printf escaped > escaped.txt", "access":"workspace_write"
            }),
        ))
        .await;
    assert_eq!(
        rejected.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!fixture.root.join("escaped.txt").exists());
    assert!(child.review().unwrap().approvals.is_empty());
    parent.revoke_command_grants().unwrap();
    deny_requested(&child, input.clone()).await;
    deny_requested(&parent, input).await;
}

#[tokio::test]
async fn command_rechecks_delegated_file_leases_after_user_approval() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", false);
    let task = submit(
        &parent,
        json!({
            "command":"printf raced > reserved.txt", "access":"workspace_write", "network_access":"allowed"
        }),
    );
    let approval = pending(&parent).await;
    assert!(!parent.review().unwrap().busy);
    fixture
        .access
        .try_acquire("child", &["reserved.txt".into()])
        .unwrap();
    parent.approve(&approval.id, true).unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(!fixture.root.join("reserved.txt").exists());
    fixture.access.release("child").await;
    assert!(!parent.review().unwrap().busy);
}

#[tokio::test]
async fn sandboxed_offline_commands_need_no_approval_in_either_reviewer_mode() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", false);
    for mode in [
        agent_local::ApprovalMode::Ask,
        agent_local::ApprovalMode::AutoReview,
    ] {
        parent.set_approval_mode(mode).unwrap();
        let status = completed(
            &parent,
            result(submit(
                &parent,
                json!({
                    "command":"printf SAFE > offline.txt; cat offline.txt", "timeout_seconds":5
                }),
            ))
            .await,
        )
        .await;
        assert_eq!(status.status, "completed", "{}", status.output);
        assert_eq!(status.output, "SAFE");
        assert!(parent.review().unwrap().approvals.is_empty());
        assert!(parent.review().unwrap().command_grants.is_empty());
    }
    let child = fixture
        .tools("child", true)
        .with_command_approval_scope("parent")
        .unwrap();
    assert_eq!(
        child.approval_mode().unwrap(),
        agent_local::ApprovalMode::AutoReview
    );
    let status = result(submit(&child, json!({
        "command":"cat offline.txt; if printf FAIL > offline.txt 2>/dev/null; then exit 87; fi", "timeout_seconds":5
    }))).await;
    assert_eq!(status.status, "completed", "{}", status.output);
    assert_eq!(
        fs::read_to_string(fixture.root.join("offline.txt")).unwrap(),
        "SAFE"
    );
}

#[tokio::test]
async fn one_session_approval_releases_existing_sibling_requests_and_revoke_blocks_stale_review() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", false);
    let child = fixture
        .tools("child", true)
        .with_command_approval_scope("parent")
        .unwrap();
    parent
        .set_approval_mode(agent_local::ApprovalMode::AutoReview)
        .unwrap();
    let input = json!({"command":"printf APPROVED", "access":"read_only", "network_access":"allowed", "timeout_seconds":5});
    let parent_task = submit(&parent, input.clone());
    let child_task = submit(&child, input.clone());
    let approval = pending(&parent).await;
    pending(&child).await;
    parent
        .approve_if_current(
            &approval.id,
            true,
            CommandApprovalScope::Session,
            approval.epoch,
        )
        .unwrap();
    assert_eq!(result(parent_task).await.status, "completed");
    assert_eq!(result(child_task).await.status, "completed");
    assert!(child.review().unwrap().approvals.is_empty());
    parent.revoke_command_grants().unwrap();
    let pending_task = submit(&child, input);
    let approval = pending(&child).await;
    parent.revoke_command_grants().unwrap();
    assert!(
        child
            .approve_if_current(
                &approval.id,
                true,
                CommandApprovalScope::Session,
                approval.epoch
            )
            .is_err()
    );
    assert!(pending_task.await.unwrap().is_err());
    assert!(parent.review().unwrap().command_grants.is_empty());
}
