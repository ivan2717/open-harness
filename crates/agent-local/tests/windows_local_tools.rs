#![cfg(windows)]

use agent_local::{CommandApprovalScope, LocalTools, PermissionMode, WorkspaceAccess};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let helper = std::env::var_os("OPENHARNESS_WINDOWS_SANDBOX_HELPER")
            .expect("build openharness-sandbox and set OPENHARNESS_WINDOWS_SANDBOX_HELPER before running Windows integration tests");
        assert!(Path::new(&helper).is_absolute() && Path::new(&helper).is_file());
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir().join(format!(
            "agent-local-windows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project with spaces");
        fs::create_dir_all(&root).unwrap();
        let state = base.join("state");
        Self { base, root, state }
    }

    fn tools(&self) -> LocalTools {
        let tools = LocalTools::open(
            self.root.clone(),
            self.state.clone(),
            PermissionMode::WorkspaceWrite,
        )
        .unwrap();
        let review = tools.review().unwrap();
        assert!(review.sandbox_available);
        assert_eq!(review.execution_mode, "sandboxed");
        tools
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "windows-call".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

async fn pending(tools: &LocalTools) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(approval) = tools.review().unwrap().approvals.first() {
                assert_eq!(approval.execution_mode, "sandboxed");
                return approval.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("command did not request approval")
}

async fn approved(tools: &LocalTools, arguments: Value) -> Value {
    let needs_approval = arguments.get("network_access").and_then(Value::as_str) == Some("allowed");
    let worker = tools.clone();
    let task = tokio::spawn(async move { worker.execute(call("run_command", arguments)).await });
    if needs_approval {
        tools.approve(&pending(tools).await, true).unwrap();
    }
    serde_json::from_str(&task.await.unwrap().unwrap()).unwrap()
}

async fn file_ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the sandboxed child never started");
}

fn delayed_child() -> &'static str {
    r#"$childShell = Join-Path $PSHOME 'pwsh.exe'; if (!(Test-Path $childShell)) { $childShell = Join-Path $PSHOME 'powershell.exe' }; & $childShell -NoLogo -NoProfile -NonInteractive -Command "[IO.File]::WriteAllText('started.txt','ready'); Start-Sleep -Seconds 3; [IO.File]::WriteAllText('late.txt','escaped')""#
}

#[tokio::test]
async fn inherited_child_writes_reuse_root_approval_and_respect_peer_leases_and_revocation() {
    let fixture = Fixture::new();
    let access = WorkspaceAccess::new(fixture.root.clone()).unwrap();
    let parent = fixture
        .tools()
        .with_collaboration(access.clone(), "parent", None)
        .unwrap();
    let child = LocalTools::open(
        fixture.root.clone(),
        fixture.base.join("child-state"),
        PermissionMode::WorkspaceWrite,
    )
    .unwrap()
    .with_collaboration(access.clone(), "child", None)
    .unwrap()
    .with_command_approval_scope("parent")
    .unwrap();
    let worker = parent.clone();
    let initial = tokio::spawn(async move {
        worker.execute(call("run_command", json!({"command":"[IO.File]::WriteAllText('parent.txt','parent')", "yield_time_ms":30000, "network_access":"allowed"}))).await
    });
    parent
        .approve_with_scope(&pending(&parent).await, true, CommandApprovalScope::Session)
        .unwrap();
    let result: Value = serde_json::from_str(&initial.await.unwrap().unwrap()).unwrap();
    assert_eq!(result["status"], "completed", "{result}");

    let result: Value = serde_json::from_str(
        &child
            .execute(call(
                "run_command",
                json!({
                    "command":"[IO.File]::WriteAllText('child.txt','child')", "yield_time_ms":30000, "network_access":"allowed"
                }),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert!(child.review().unwrap().approvals.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.root.join("child.txt")).unwrap(),
        "child"
    );

    access
        .acquire("peer", &["reserved.txt".into()])
        .await
        .unwrap();
    assert_eq!(
        child
            .execute(call(
                "run_command",
                json!({
                    "command":"[IO.File]::WriteAllText('reserved.txt','invalid')"
                })
            ))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!fixture.root.join("reserved.txt").exists());
    access.release("peer").await;

    child.revoke_command_grants().unwrap();
    assert!(parent.review().unwrap().command_grants.is_empty());
    for tools in [parent, child] {
        let worker = tools.clone();
        let denied = tokio::spawn(async move {
            worker
                .execute(call(
                    "run_command",
                    json!({"command":"[IO.File]::WriteAllText('denied.txt','invalid')", "network_access":"allowed"}),
                ))
                .await
        });
        tools.approve(&pending(&tools).await, false).unwrap();
        assert!(denied.await.unwrap().is_err());
    }
    assert!(!fixture.root.join("denied.txt").exists());
}

#[tokio::test]
async fn approval_is_required_single_use_and_cancellable_before_execution() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"[IO.File]::WriteAllText('denied.txt','unexpected')", "network_access":"allowed"}),
            ))
            .await
    });
    let id = pending(&tools).await;
    assert!(!tools.review().unwrap().busy);
    assert!(!fixture.root.join("denied.txt").exists());
    tools.approve(&id, false).unwrap();
    assert!(tools.approve(&id, true).is_err());
    assert!(task.await.unwrap().is_err());
    assert!(!fixture.root.join("denied.txt").exists());
    assert_eq!(
        tools.review().unwrap().commands.last().unwrap().status,
        "denied"
    );

    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"[IO.File]::WriteAllText('cancelled.txt','unexpected')", "network_access":"allowed"}),
            ))
            .await
    });
    pending(&tools).await;
    task.abort();
    let _ = task.await;
    let review = tools.review().unwrap();
    assert!(review.approvals.is_empty());
    assert!(!review.busy);
    assert_eq!(review.commands.last().unwrap().status, "cancelled");
    assert!(!fixture.root.join("cancelled.txt").exists());
}

#[tokio::test]
async fn approved_unicode_output_and_command_changes_survive_reopening() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("result.txt"), "user's existing work").unwrap();
    let tools = fixture.tools();
    let result = approved(
        &tools,
        json!({"command":"[IO.File]::WriteAllText('result.txt','用户修改 + agent'); [Console]::Write('你好')", "yield_time_ms":30000}),
    )
    .await;
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(result["output"], "你好");
    drop(tools);
    let restored = fixture.tools();
    let review = restored.review().unwrap();
    assert_eq!(review.commands.last().unwrap().status, "completed");
    assert_eq!(review.commands.last().unwrap().output, "你好");
    assert_eq!(review.changes.len(), 1);
    assert_eq!(
        review.changes[0].before.as_deref(),
        Some("user's existing work")
    );
    assert_eq!(review.changes[0].after.as_deref(), Some("用户修改 + agent"));
    assert!(!review.changes[0].revertible);
    assert!(restored.revert("result.txt").is_err());
}

#[tokio::test]
async fn yielded_output_preserves_cursors_and_workspace_exclusivity() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let result = approved(
        &tools,
        json!({"command":"[Console]::Write('first'); Start-Sleep -Seconds 1; [Console]::Write('second'); [IO.File]::WriteAllText('result.txt','done')", "yield_time_ms":0}),
    )
    .await;
    assert_eq!(result["status"], "running", "{result}");
    assert_eq!(
        tools
            .execute(call("apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: competing.txt\n+not allowed\n*** End Patch"})))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let id = result["command_id"].as_str().unwrap();
    let mut cursor = result["next_cursor"].as_u64().unwrap();
    let mut output = result["output"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let peek = tools.command_status(id, cursor, 0).await.unwrap();
            let _ = tools.review().unwrap();
            let update = tools.command_status(id, cursor, 5000).await.unwrap();
            assert!(update.output.starts_with(&peek.output));
            output.push_str(&update.output);
            cursor = update.next_cursor;
            if update.status != "running" && !update.has_more_output {
                assert_eq!(update.status, "completed", "{output}");
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(output, "firstsecond");
    assert!(!tools.commands_running());
    assert_eq!(
        fs::read_to_string(fixture.root.join("result.txt")).unwrap(),
        "done"
    );
}

#[tokio::test]
async fn cancellation_stops_an_actual_powershell_descendant_and_preserves_review() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let tools = fixture.tools();
    let result = approved(
        &tools,
        json!({"command":delayed_child(), "yield_time_ms":0}),
    )
    .await;
    let id = result["command_id"].as_str().unwrap();
    file_ready(&fixture.root.join("started.txt")).await;
    assert_eq!(
        other
            .tools()
            .command_status(id, 0, 0)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(tools.stop_command(id).await.unwrap().status, "cancelled");
    assert!(!tools.commands_running());
    assert!(!tools.review().unwrap().busy);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!fixture.root.join("late.txt").exists());
    assert!(
        tools
            .review()
            .unwrap()
            .changes
            .iter()
            .any(|change| change.path == "started.txt")
    );
}

#[tokio::test]
async fn execution_deadline_kills_the_windows_job() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let result = approved(
        &tools,
        json!({"command":delayed_child(), "timeout_seconds":1, "yield_time_ms":30000}),
    )
    .await;
    assert_eq!(result["status"], "timed_out", "{result}");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!fixture.root.join("late.txt").exists());
    assert!(!tools.review().unwrap().busy);
}

#[tokio::test]
async fn windows_commands_reuse_runtime_inventory_without_per_conversation_packages() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let inventory: Value = serde_json::from_str(
        &tools
            .execute(call("load_workspace_dependencies", json!({})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(inventory.get("dependency_environment").is_none());
    assert!(!fixture.root.join(".openharness-tmp").exists());
    let result = approved(
        &tools,
        json!({"command":r#"$probe = Join-Path $env:TEMP 'probe.txt'; [IO.File]::WriteAllText($probe,'scratch'); @{temporary=$env:TEMP; tmp=$env:TMP; localAppData=$env:LOCALAPPDATA; hostScratch=$env:TMPDIR; probe=[IO.File]::ReadAllText($probe); cache=$env:PIP_CACHE_DIR; store=$env:pnpm_config_store_dir; requireVenv=$env:PIP_REQUIRE_VIRTUALENV} | ConvertTo-Json -Compress"#, "access":"workspace_write", "yield_time_ms":30000}),
    )
    .await;
    assert_eq!(result["status"], "completed", "{result}");
    let environment: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
    // Windows redirects TEMP/TMP into the per-command AppContainer profile;
    // TMPDIR still names the host's workspace scratch. Neither is a package
    // environment. Profile cleanup removes TEMP after the command exits.
    let expected_tmp = fs::canonicalize(&fixture.root)
        .unwrap()
        .join(".openharness-tmp/tmp");
    assert_eq!(
        fs::canonicalize(environment["hostScratch"].as_str().unwrap()).unwrap(),
        expected_tmp
    );
    let temporary = PathBuf::from(environment["temporary"].as_str().unwrap());
    assert!(temporary.is_absolute());
    assert_eq!(environment["temporary"], environment["tmp"]);
    assert_eq!(
        temporary,
        PathBuf::from(environment["localAppData"].as_str().unwrap()).join("Temp")
    );
    assert!(!temporary.starts_with(fs::canonicalize(&fixture.root).unwrap()));
    assert_eq!(environment["probe"], "scratch");
    assert!(
        !temporary.exists(),
        "completed command retained its AppContainer scratch directory"
    );
    assert!(environment["cache"].is_null());
    assert!(environment["store"].is_null());
    assert!(environment["requireVenv"].is_null());
    for path in [
        ".openharness-tmp/tasks",
        "node_modules",
        "package.json",
        ".venv",
    ] {
        assert!(
            !fixture.root.join(path).exists(),
            "unexpected dependency environment: {path}"
        );
    }
    assert!(tools.review().unwrap().changes.is_empty());
}

#[test]
fn windows_command_host_for_crash_test() {
    let Some(base) = std::env::var_os("OPENHARNESS_TEST_WINDOWS_COMMAND_HOST") else {
        return;
    };
    let base = PathBuf::from(base);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let tools = LocalTools::open(
                base.join("project with spaces"),
                base.join("state"),
                PermissionMode::WorkspaceWrite,
            )
            .unwrap();
            approved(
                &tools,
                json!({"command":delayed_child(), "yield_time_ms":30000}),
            )
            .await;
        });
}

#[tokio::test]
async fn terminating_the_host_stops_descendants_and_marks_the_journal_interrupted() {
    let fixture = Fixture::new();
    drop(fixture.tools());
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "windows_command_host_for_crash_test",
            "--nocapture",
        ])
        .env("OPENHARNESS_TEST_WINDOWS_COMMAND_HOST", &fixture.base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        while !fixture.root.join("started.txt").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    host.kill().unwrap();
    host.wait().unwrap();
    ready.expect("the sandboxed host never started its child");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!fixture.root.join("late.txt").exists());
    assert_eq!(
        fixture
            .tools()
            .review()
            .unwrap()
            .commands
            .last()
            .unwrap()
            .status,
        "interrupted"
    );
}
