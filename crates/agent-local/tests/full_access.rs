use agent_local::{ApprovalMode, LocalTools, NetworkAccess, PermissionMode, WorkspaceAccess};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "openharness-full-access-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("project")).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
    fn tools(&self, name: &str, mode: PermissionMode) -> LocalTools {
        LocalTools::open(self.0.join("project"), self.0.join(name), mode).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "full-access-test".into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}
async fn execute(tools: &LocalTools, name: &str, args: Value) -> Value {
    serde_json::from_str(&tools.execute(call(name, args)).await.unwrap()).unwrap()
}

#[tokio::test]
async fn host_files_are_explicit_and_never_disguised_as_project_undo() {
    let f = Fixture::new();
    let outside = f.0.join("outside.txt");
    fs::write(&outside, "outside original\n").unwrap();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    assert_eq!(tools.approval_mode().unwrap(), ApprovalMode::Ask);
    assert!(
        tools
            .execute(call("read_file", json!({"path":outside})))
            .await
            .is_err()
    );
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    assert!(tools.full_access());
    assert_eq!(tools.review().unwrap().execution_mode, "unrestricted");
    assert!(
        tools
            .instructions()
            .unwrap()
            .contains("outside this directory")
    );
    assert_eq!(
        execute(&tools, "read_file", json!({"path":outside})).await["content"],
        "outside original\n"
    );
    let result = execute(&tools, "apply_patch", json!({"patch":format!("*** Begin Patch\n*** Update File: {}\n@@\n-outside original\n+outside revised\n*** Add File: ordinary.txt\n+project edit\n*** End Patch", outside.display())})).await;
    assert_eq!(result["files"][0]["project_undo"], false);
    assert_eq!(result["files"][1]["project_undo"], true);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside revised\n");
    let changes = tools.review().unwrap().changes;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "ordinary.txt");
    assert!(tools.revert(outside.to_str().unwrap()).is_err());
    tools.revert("ordinary.txt").unwrap();
    assert!(!f.0.join("project/ordinary.txt").exists());
    // Protected project paths are available only in FullAccess and cannot be
    // fed back through the safe project undo resolver.
    execute(
        &tools,
        "apply_patch",
        json!({"patch":"*** Begin Patch\n*** Add File: .env\n+fixture-only=value\n*** End Patch"}),
    )
    .await;
    assert!(tools.review().unwrap().changes.is_empty());
    assert!(tools.revert(".env").is_err());
    // Reader, search and legacy write all use the same effective host scope.
    execute(
        &tools,
        "write_file",
        json!({"path":f.0.join("new/sub/text.txt"),"content":"needle","expected_content":null}),
    )
    .await;
    assert_eq!(
        execute(
            &tools,
            "search_files",
            json!({"path":f.0.join("new"),"query":"needle"})
        )
        .await["matches"][0]["text"],
        "needle"
    );
    tools.set_approval_mode(ApprovalMode::Ask).unwrap();
    assert!(!tools.full_access());
    assert!(
        tools
            .execute(call("read_file", json!({"path":outside})))
            .await
            .is_err()
    );
    assert!(
        tools
            .execute(call("read_file", json!({"path":".env"})))
            .await
            .is_err()
    );
    assert!(
        !tools
            .definitions()
            .iter()
            .find(|d| d.name == "run_command")
            .is_some_and(|d| d.description.contains("full user access"))
    );
}

#[tokio::test]
async fn full_mode_inherits_but_does_not_override_explicit_child_restrictions() {
    let f = Fixture::new();
    let access = WorkspaceAccess::new(f.0.join("project")).unwrap();
    let root = f
        .tools("root", PermissionMode::WorkspaceWrite)
        .with_collaboration(access.clone(), "root", None)
        .unwrap();
    let inherited = f
        .tools("child", PermissionMode::WorkspaceWrite)
        .with_collaboration(access.clone(), "child", None)
        .unwrap()
        .with_command_approval_scope("root")
        .unwrap();
    let readonly = f
        .tools("readonly", PermissionMode::ReadOnly)
        .with_collaboration(access.clone(), "readonly", None)
        .unwrap()
        .with_command_approval_scope("root")
        .unwrap();
    access
        .acquire("leased", &["owned.txt".into()])
        .await
        .unwrap();
    let leased = f
        .tools("leased", PermissionMode::WorkspaceWrite)
        .with_collaboration(access.clone(), "leased", Some(vec!["owned.txt".into()]))
        .unwrap()
        .with_command_approval_scope("root")
        .unwrap();
    let unrelated = f
        .tools("unrelated", PermissionMode::WorkspaceWrite)
        .with_collaboration(access, "unrelated", None)
        .unwrap();
    root.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    assert!(inherited.full_access());
    assert!(!readonly.full_access());
    assert!(!leased.full_access());
    assert!(!unrelated.full_access());
    fs::write(f.0.join("outside.txt"), "outside").unwrap();
    for tools in [&readonly, &leased, &unrelated] {
        assert!(
            tools
                .execute(call("read_file", json!({"path":f.0.join("outside.txt")})))
                .await
                .is_err()
        );
        assert!(tools.execute(call("apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: ../outside-new.txt\n+denied\n*** End Patch"}))).await.is_err());
    }
    execute(
        &leased,
        "apply_patch",
        json!({"patch":"*** Begin Patch\n*** Add File: owned.txt\n+allowed\n*** End Patch"}),
    )
    .await;
    assert!(
        leased
            .execute(call(
                "apply_patch",
                json!({"patch":"*** Begin Patch\n*** Add File: other.txt\n+denied\n*** End Patch"})
            ))
            .await
            .is_err()
    );
    root.set_approval_mode(ApprovalMode::AutoReview).unwrap();
    assert!(!inherited.full_access());
    assert_eq!(inherited.approval_mode().unwrap(), ApprovalMode::AutoReview);
}

async fn command(tools: &LocalTools, args: Value) -> Value {
    let initial =
        tokio::time::timeout(Duration::from_secs(15), execute(tools, "run_command", args))
            .await
            .expect("unexpected approval or stalled command");
    let mut result = initial;
    for _ in 0..20 {
        if result["status"] != "running" {
            return result;
        }
        result = execute(
            tools,
            "manage_command",
            json!({"command_id":result["command_id"], "wait_ms":1000}),
        )
        .await;
    }
    panic!("command never completed: {result}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn explicit_full_access_runs_host_commands_without_sandbox_or_extra_approval() {
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    fs::write(f.0.join("outside.txt"), "HOST_READ_OK").unwrap();
    #[cfg(unix)]
    let script = "cat ../outside.txt; printf HOST_WRITE_OK > ../host-created.txt";
    #[cfg(windows)]
    let script = "Get-Content ../outside.txt; [IO.File]::WriteAllText((Join-Path (Get-Location) '../host-created.txt'), 'HOST_WRITE_OK')";
    let result = command(&tools, json!({"command":script,"timeout_seconds":10})).await;
    assert_eq!(result["status"], "completed", "{result}");
    assert!(result["output"].as_str().unwrap().contains("HOST_READ_OK"));
    assert_eq!(
        fs::read_to_string(f.0.join("host-created.txt")).unwrap(),
        "HOST_WRITE_OK"
    );
    let review = tools.review().unwrap();
    assert_eq!(
        review.commands.last().unwrap().network_access,
        NetworkAccess::Allowed
    );
    assert_eq!(
        review.commands.last().unwrap().execution_mode,
        "unrestricted"
    );
    assert!(review.approvals.is_empty());
    assert!(review.command_grants.is_empty());
    assert!(review.changes.is_empty());
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn explicit_restrictions_use_real_sandbox_even_in_full_mode() {
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    // On Windows this requires the packaged sandbox helper; missing helper
    // must fail closed rather than execute a restricted command unrestricted.
    if !tools.review().unwrap().sandbox_available {
        assert!(
            tools
                .execute(call(
                    "run_command",
                    json!({"command":"echo rejected", "network_access":"denied"})
                ))
                .await
                .is_err()
        );
        return;
    }
    #[cfg(unix)]
    let script =
        "if printf escaped > ../denied.txt 2>/dev/null; then exit 12; fi; printf RESTRICTED_OK";
    #[cfg(windows)]
    let script = "try { [IO.File]::WriteAllText((Join-Path (Get-Location) '../denied.txt'), 'escape'); exit 12 } catch {}; Write-Output RESTRICTED_OK";
    for extras in [
        json!({"access":"read_only"}),
        json!({"network_access":"denied"}),
    ] {
        let mut args = extras;
        args["command"] = script.into();
        args["timeout_seconds"] = 10.into();
        let result = command(&tools, args).await;
        assert_eq!(result["status"], "completed", "{result}");
        assert!(!f.0.join("denied.txt").exists());
        assert_eq!(
            tools
                .review()
                .unwrap()
                .commands
                .last()
                .unwrap()
                .execution_mode,
            "sandboxed"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn host_command_cancellation_stops_its_descendants() {
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    let result = execute(&tools, "run_command", json!({"command":"(sleep 2; printf leaked > ../late-child.txt) & wait", "yield_time_ms":20})).await;
    assert_eq!(result["status"], "running");
    execute(
        &tools,
        "manage_command",
        json!({"command_id":result["command_id"],"action":"stop","wait_ms":1000}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(!f.0.join("late-child.txt").exists());
    let review = tools.review().unwrap();
    assert_eq!(review.commands.last().unwrap().status, "cancelled");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn full_access_network_is_really_enabled_without_a_grant() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let serve = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 4096];
                let _ = stream.read(&mut bytes);
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nHOST_NETWORK").unwrap();
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "host network never connected"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    #[cfg(unix)]
    let script = format!("curl --silent --show-error --max-time 5 {endpoint}");
    #[cfg(windows)]
    let script = format!("(Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 '{endpoint}').Content");
    let result = command(&tools, json!({"command":script,"timeout_seconds":8})).await;
    serve.join().unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert!(result["output"].as_str().unwrap().contains("HOST_NETWORK"));
    assert!(tools.pending_approvals().unwrap().is_empty());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn full_mode_downgrade_restores_sandbox_and_network_approval() {
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    tools.set_approval_mode(ApprovalMode::Ask).unwrap();
    let result = command(&tools, json!({"command":"if printf escaped > ../escaped.txt 2>/dev/null; then exit 31; fi; printf SANDBOX_OK", "timeout_seconds":5})).await;
    assert_eq!(result["status"], "completed", "{result}");
    assert!(!f.0.join("escaped.txt").exists());
    assert_eq!(
        tools
            .review()
            .unwrap()
            .commands
            .last()
            .unwrap()
            .network_access,
        NetworkAccess::Denied
    );
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"printf SHOULD_NOT_EXECUTE", "network_access":"allowed"}),
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = tools.pending_approvals().unwrap().first() {
                assert_eq!(approval.execution_mode, "sandboxed");
                tools.approve(&approval.id, false).unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(task.await.unwrap().is_err());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn file_scoped_child_of_full_access_parent_uses_read_only_os_sandbox() {
    let f = Fixture::new();
    let access = WorkspaceAccess::new(f.0.join("project")).unwrap();
    let root = f
        .tools("root", PermissionMode::WorkspaceWrite)
        .with_collaboration(access.clone(), "root", None)
        .unwrap();
    let child = f
        .tools("child", PermissionMode::WorkspaceWrite)
        .with_collaboration(access, "child", Some(Vec::new()))
        .unwrap()
        .with_command_approval_scope("root")
        .unwrap();
    root.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    let result = command(&child, json!({"command":"if printf escaped > ../outside.txt 2>/dev/null; then exit 31; fi; if printf changed > project-new.txt 2>/dev/null; then exit 32; fi; printf CHILD_SANDBOX_OK", "timeout_seconds":5})).await;
    assert_eq!(result["status"], "completed", "{result}");
    assert!(!f.0.join("outside.txt").exists());
    assert!(!f.0.join("project/project-new.txt").exists());
    assert_eq!(child.review().unwrap().execution_mode, "sandboxed");
    assert_eq!(
        child
            .review()
            .unwrap()
            .commands
            .last()
            .unwrap()
            .network_access,
        NetworkAccess::Denied
    );
}

#[cfg(unix)]
#[tokio::test]
async fn failed_mixed_host_patch_rolls_back_external_edits_without_project_journal_entries() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let tools = f.tools("state", PermissionMode::WorkspaceWrite);
    tools.set_approval_mode(ApprovalMode::FullAccess).unwrap();
    fs::write(f.0.join("outside.txt"), "before\n").unwrap();
    let locked = f.0.join("project/locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(locked.join("probe"), "probe").is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        return; // A root runner can bypass this intentional OS write failure.
    }
    let result = tools.execute(call("apply_patch", json!({"patch":format!("*** Begin Patch\n*** Update File: {}\n@@\n-before\n+after\n*** Add File: ordinary.txt\n+temporary\n*** Add File: locked/failure.txt\n+blocked\n*** End Patch", f.0.join("outside.txt").display())}))).await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.unwrap_err().to_string().contains("rolled back"));
    assert_eq!(
        fs::read_to_string(f.0.join("outside.txt")).unwrap(),
        "before\n"
    );
    assert!(!f.0.join("project/ordinary.txt").exists());
    assert!(tools.review().unwrap().changes.is_empty());
}
