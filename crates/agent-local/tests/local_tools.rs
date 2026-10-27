use agent_local::{LocalTools, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use std::time::Duration;
use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir().join(format!(
            "agent-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project");
        let state = base.join("state");
        fs::create_dir_all(&root).unwrap();
        Self { base, root, state }
    }
    fn tools(&self, mode: PermissionMode) -> LocalTools {
        LocalTools::open(self.root.clone(), self.state.clone(), mode).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "test-call".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

#[tokio::test]
async fn local_file_edits_preserve_existing_user_changes_and_survive_restart() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("main.txt"), "user's uncommitted work\n").unwrap();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    tools
        .execute(call(
            "edit_file",
            json!({"path":"main.txt","old_text":"work","new_text":"work plus agent fix"}),
        ))
        .await
        .unwrap();
    let changes = tools.review().unwrap().changes;
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].before.as_deref(),
        Some("user's uncommitted work\n")
    );
    assert!(changes[0].revertible);
    drop(tools);
    let restored = fixture.tools(PermissionMode::WorkspaceWrite);
    restored.revert("main.txt").unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("main.txt")).unwrap(),
        "user's uncommitted work\n"
    );
    assert!(restored.review().unwrap().changes.is_empty());
}

#[tokio::test]
async fn revert_refuses_changes_made_after_the_agent_edit() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    tools
        .execute(call(
            "write_file",
            json!({"path":"new.txt","content":"agent","expected_content":null}),
        ))
        .await
        .unwrap();
    fs::write(fixture.root.join("new.txt"), "user changed it").unwrap();
    assert!(!tools.review().unwrap().changes[0].revertible);
    assert_eq!(
        tools.revert("new.txt").unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("new.txt")).unwrap(),
        "user changed it"
    );
}

#[tokio::test]
async fn paths_permissions_and_expected_contents_are_enforced() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("data.txt"), "original").unwrap();
    let read_only = fixture.tools(PermissionMode::ReadOnly);
    assert!(
        read_only
            .execute(call("read_file", json!({"path":"data.txt"})))
            .await
            .is_ok()
    );
    assert!(
        read_only
            .execute(call(
                "write_file",
                json!({"path":"data.txt","content":"replacement","expected_content":"original"})
            ))
            .await
            .is_err()
    );
    assert_eq!(
        read_only
            .execute(call(
                "run_command",
                json!({"command":"echo unsafe", "access":"workspace_write"})
            ))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for path in [
        "../outside",
        "/tmp/outside",
        ".git/config",
        ".env",
        "nested/.env.local",
        ".ssh/id_rsa",
    ] {
        assert!(
            tools
                .execute(call("read_file", json!({"path":path})))
                .await
                .is_err(),
            "{path}"
        );
        assert!(
            tools
                .execute(call(
                    "write_file",
                    json!({"path":path,"content":"bad","expected_content":null})
                ))
                .await
                .is_err(),
            "{path}"
        );
    }
    assert!(
        tools
            .execute(call(
                "write_file",
                json!({"path":"new.txt","content":"new"})
            ))
            .await
            .is_err()
    );
    assert!(
        tools
            .execute(call(
                "write_file",
                json!({"path":"data.txt","content":"replacement","expected_content":"stale"})
            ))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("data.txt")).unwrap(),
        "original"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symbolic_links_and_hard_links_cannot_expose_external_files() {
    let fixture = Fixture::new();
    fs::write(fixture.base.join("secret"), "secret contents").unwrap();
    std::os::unix::fs::symlink(&fixture.base, fixture.root.join("linked")).unwrap();
    fs::hard_link(fixture.base.join("secret"), fixture.root.join("hard-link")).unwrap();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for path in ["linked/secret", "hard-link"] {
        assert!(
            tools
                .execute(call("read_file", json!({"path":path})))
                .await
                .is_err()
        );
        assert!(
            tools
                .execute(call(
                    "write_file",
                    json!({"path":path,"content":"bad","expected_content":"secret contents"})
                ))
                .await
                .is_err()
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.base.join("secret")).unwrap(),
        "secret contents"
    );
}

#[tokio::test]
async fn read_pagination_and_search_stay_bounded() {
    let fixture = Fixture::new();
    let content = "汉字 abc\n".repeat(4000);
    fs::write(fixture.root.join("large.txt"), &content).unwrap();
    fs::create_dir(fixture.root.join("node_modules")).unwrap();
    fs::write(fixture.root.join("node_modules/secret.txt"), "needle").unwrap();
    let tools = fixture.tools(PermissionMode::ReadOnly);
    let mut accumulated = String::new();
    let mut offset = 0;
    loop {
        let value: Value = serde_json::from_str(
            &tools
                .execute(call(
                    "read_file",
                    json!({"path":"large.txt","offset":offset}),
                ))
                .await
                .unwrap(),
        )
        .unwrap();
        accumulated.push_str(value["content"].as_str().unwrap());
        let Some(next) = value["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(accumulated, content);
    let value: Value = serde_json::from_str(
        &tools
            .execute(call("search_files", json!({"query":"needle","path":"."})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["matches"].as_array().unwrap().len(), 0);
}

#[test]
fn workspace_and_journal_identity_are_checked() {
    let fixture = Fixture::new();
    assert!(
        LocalTools::open(
            fixture.root.clone(),
            fixture.root.join("state"),
            PermissionMode::ReadOnly
        )
        .is_err()
    );
    let tools = fixture.tools(PermissionMode::ReadOnly);
    drop(tools);
    let other = fixture.base.join("other");
    fs::create_dir(&other).unwrap();
    assert!(LocalTools::open(other, fixture.state.clone(), PermissionMode::ReadOnly).is_err());
    let protected = fixture.base.join(".aws").join("nested");
    fs::create_dir_all(&protected).unwrap();
    assert!(LocalTools::open(protected, fixture.state.clone(), PermissionMode::ReadOnly).is_err());
    let inside_state = fixture.state.join("project");
    fs::create_dir_all(&inside_state).unwrap();
    assert!(
        LocalTools::open(
            inside_state,
            fixture.state.clone(),
            PermissionMode::ReadOnly
        )
        .is_err()
    );
    if let Some(home) = std::env::var_os("HOME") {
        assert!(
            LocalTools::open(home.into(), fixture.state.clone(), PermissionMode::ReadOnly).is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_the_workspace_root_invalidates_existing_access() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    let original = fixture.base.join("original");
    fs::rename(&fixture.root, &original).unwrap();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside, &fixture.root).unwrap();
    assert!(
        tools
            .execute(call("read_file", json!({"path":"secret"})))
            .await
            .is_err()
    );
    assert!(tools.review().is_err());
    assert!(tools.revert("secret").is_err());
    fs::remove_file(&fixture.root).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    assert!(
        tools
            .execute(call("list_files", json!({"path":"."})))
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn reopening_refuses_a_replaced_workspace_and_accepts_legacy_journals() {
    let fixture = Fixture::new();
    drop(fixture.tools(PermissionMode::WorkspaceWrite));
    let journal_path = fixture.state.join("journal.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
    assert!(journal["root_identity"]["inode"].is_u64());
    journal.as_object_mut().unwrap().remove("root_identity");
    fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    drop(fixture.tools(PermissionMode::WorkspaceWrite));
    let migrated: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
    assert!(migrated["root_identity"]["inode"].is_u64());
    fs::rename(&fixture.root, fixture.base.join("original-project")).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    let result = LocalTools::open(
        fixture.root.clone(),
        fixture.state.clone(),
        PermissionMode::WorkspaceWrite,
    );
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
}

#[cfg(target_os = "macos")]
#[test]
fn command_host_for_crash_test() {
    let Some(base) = std::env::var_os("OPENHARNESS_TEST_COMMAND_HOST") else {
        return;
    };
    let base = PathBuf::from(base);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let tools = LocalTools::open(base.join("project"), base.join("state"), PermissionMode::WorkspaceWrite).unwrap();
        let worker = tools.clone();
        let task = tokio::spawn(async move { worker.execute(call("run_command", json!({"command":"printf started > started.txt; sleep 2; printf escaped > late.txt"}))).await });
        let id = pending(&tools).await;
        tools.approve(&id, true).unwrap();
        let _ = task.await;
    });
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn killing_the_host_does_not_leave_a_foreground_command_running() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    drop(tools);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "command_host_for_crash_test", "--nocapture"])
        .env("OPENHARNESS_TEST_COMMAND_HOST", &fixture.base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.join("started.txt").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    child.kill().unwrap();
    child.wait().unwrap();
    started.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!fixture.root.join("late.txt").exists());
    let restored = fixture.tools(PermissionMode::WorkspaceWrite);
    assert_eq!(
        restored.review().unwrap().commands.last().unwrap().status,
        "interrupted"
    );
}

#[cfg(target_os = "macos")]
async fn pending(tools: &LocalTools) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = tools.review().unwrap().approvals.first() {
                return approval.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn command_approval_is_single_use_and_cancellation_clears_it() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"touch should-not-exist"}),
            ))
            .await
    });
    let id = pending(&tools).await;
    assert!(!fixture.root.join("should-not-exist").exists());
    assert!(!tools.review().unwrap().busy);
    tools.approve(&id, false).unwrap();
    assert!(tools.approve(&id, true).is_err());
    assert!(task.await.unwrap().is_err());
    assert!(!fixture.root.join("should-not-exist").exists());
    assert!(tools.review().unwrap().approvals.is_empty());
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"touch should-not-exist"}),
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
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn approved_command_is_sandboxed_and_changes_are_reviewed() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"printf hello > command.txt; printf result"}),
            ))
            .await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    let result = task.await.unwrap().unwrap();
    let value: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(value["status"], "completed", "{result}");
    assert_eq!(value["output"], "result");
    let review = tools.review().unwrap();
    assert_eq!(review.changes.len(), 1);
    assert_eq!(review.changes[0].after.as_deref(), Some("hello"));
    assert!(!review.changes[0].revertible);
    assert!(tools.revert("command.txt").is_err());
    let outside = fixture.base.join("outside.txt");
    let command = format!("printf forbidden > '{}'", outside.display());
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call("run_command", json!({"command":command})))
            .await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    let result: Value = serde_json::from_str(&task.await.unwrap().unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert!(!outside.exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn commands_reuse_shared_runtime_without_creating_conversation_dependencies() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let inventory: Value = serde_json::from_str(
        &tools
            .execute(call("load_workspace_dependencies", json!({})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(inventory.get("dependency_environment").is_none());
    assert!(
        !fixture.root.join(".openharness-tmp").exists(),
        "discovery must remain read-only"
    );
    let result = approved_session(&tools, r#"printf '%s\n' "$HOME" "$TMPDIR" "$PIP_CACHE_DIR" "$pnpm_config_store_dir" "$PIP_REQUIRE_VIRTUALENV"; printf scratch > "$TMPDIR/probe.txt""#, 10_000).await;
    assert_eq!(result["status"], "completed", "{result}");
    let lines = result["output"]
        .as_str()
        .unwrap()
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], std::env::var("HOME").unwrap_or_default());
    let temporary = fs::canonicalize(&fixture.root)
        .unwrap()
        .join(".openharness-tmp/tmp");
    assert_eq!(PathBuf::from(lines[1]), temporary);
    assert!(lines[2..].iter().all(|line| line.is_empty()));
    assert!(temporary.join("probe.txt").is_file());
    for path in [
        ".openharness-tmp/tasks",
        "node_modules",
        "package.json",
        ".venv",
    ] {
        assert!(
            !fixture.root.join(path).exists(),
            "unexpected default dependency: {path}"
        );
    }
    assert!(tools.review().unwrap().changes.is_empty());

    // Old dependencies survive upgrading, but discovery never selects them.
    let legacy = fixture
        .root
        .join(".openharness-tmp/tasks/old/dependencies/python");
    fs::create_dir_all(legacy.join("bin")).unwrap();
    fs::write(legacy.join("pyvenv.cfg"), "old configuration").unwrap();
    fs::write(legacy.join("bin/python"), "old executable").unwrap();
    let other = LocalTools::open(
        fixture.root.clone(),
        fixture.base.join("another-conversation"),
        PermissionMode::WorkspaceWrite,
    )
    .unwrap();
    let other_inventory: Value = serde_json::from_str(
        &other
            .execute(call("load_workspace_dependencies", json!({})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        inventory, other_inventory,
        "runtime selection must not depend on conversation or legacy environments"
    );
    assert_eq!(
        fs::read_to_string(legacy.join("pyvenv.cfg")).unwrap(),
        "old configuration"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn timeout_kills_the_shells_child_process() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"sleep 3; printf escaped > late.txt","timeout_seconds":1}),
            ))
            .await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    let result: Value = serde_json::from_str(&task.await.unwrap().unwrap()).unwrap();
    assert_eq!(result["status"], "timed_out");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.root.join("late.txt").exists());
    assert!(!tools.review().unwrap().busy);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn dropping_execution_stops_command_descendants() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker.execute(call("run_command", json!({"command":"printf started > started.txt; sleep 2; printf escaped > late.txt"}))).await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.join("started.txt").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!fixture.root.join("late.txt").exists());
    let review = tools.review().unwrap();
    assert_eq!(review.commands.last().unwrap().status, "cancelled");
    assert!(
        review
            .changes
            .iter()
            .any(|change| change.path == "started.txt")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn command_sandbox_denies_secrets_and_network() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(".env"), "SECRET_MUST_NOT_BE_RETURNED").unwrap();
    for path in [".ENV", "FOO.KEY", "id_rsa", "id_ed25519.pub"] {
        fs::write(fixture.root.join(path), "SECRET_MUST_NOT_BE_RETURNED").unwrap();
    }
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":"cat .env .ENV FOO.KEY id_rsa id_ed25519.pub"}),
            ))
            .await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    let result = task.await.unwrap().unwrap();
    assert!(!result.contains("SECRET_MUST_NOT_BE_RETURNED"));
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "failed");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let command = format!(
        "/usr/bin/curl --silent --show-error --max-time 2 http://{}/",
        listener.local_addr().unwrap()
    );
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(call("run_command", json!({"command":command})))
            .await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    let result: Value = serde_json::from_str(&task.await.unwrap().unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn incomplete_command_scan_recovers_without_losing_original_changes() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("original.txt"), "before").unwrap();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker.execute(call("run_command", json!({"command":"printf changed > original.txt; mkdir generated; i=0; while [ $i -lt 4100 ]; do : > generated/file-$i; i=$((i+1)); done"}))).await
    });
    let id = pending(&tools).await;
    tools.approve(&id, true).unwrap();
    task.await.unwrap().unwrap();
    assert!(tools.review().unwrap().review_incomplete);
    drop(tools);
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    assert!(tools.review().unwrap().review_incomplete);
    fs::remove_dir_all(fixture.root.join("generated")).unwrap();
    let review = tools.review().unwrap();
    assert!(!review.review_incomplete);
    let change = review
        .changes
        .iter()
        .find(|change| change.path == "original.txt")
        .unwrap();
    assert_eq!(change.before.as_deref(), Some("before"));
    assert_eq!(change.after.as_deref(), Some("changed"));
    assert!(!change.revertible);
}

#[cfg(target_os = "macos")]
async fn approved_session(tools: &LocalTools, command: &str, yield_ms: u64) -> Value {
    let worker = tools.clone();
    let command = command.to_owned();
    let task = tokio::spawn(async move {
        worker
            .execute(call(
                "run_command",
                json!({"command":command,"yield_time_ms":yield_ms}),
            ))
            .await
    });
    let id = pending(tools).await;
    tools.approve(&id, true).unwrap();
    serde_json::from_str(&task.await.unwrap().unwrap()).unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn yielded_command_continues_with_incremental_output_and_exclusive_workspace() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let started = approved_session(
        &tools,
        "printf first; sleep 1; printf second; printf result > generated.txt",
        0,
    )
    .await;
    assert_eq!(started["status"], "running");
    let id = started["command_id"].as_str().unwrap();
    assert!(tools.commands_running());
    assert_eq!(
        tools
            .execute(call(
                "apply_patch",
                json!({"patch":"*** Begin Patch\n*** Add File: competing.txt\n+x\n*** End Patch"})
            ))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let mut cursor = started["next_cursor"].as_u64().unwrap();
    let mut output = started["output"].as_str().unwrap().to_owned();
    loop {
        // The UI's review polling must not consume the model's output cursor.
        let peek = tools.command_status(id, cursor, 0).await.unwrap();
        let _ = tools.review().unwrap();
        let update = tools.command_status(id, cursor, 5000).await.unwrap();
        assert!(update.output.starts_with(&peek.output));
        output.push_str(&update.output);
        cursor = update.next_cursor;
        if update.status != "running" && !update.has_more_output {
            assert_eq!(update.status, "completed");
            break;
        }
    }
    assert_eq!(output, "firstsecond");
    assert_eq!(
        fs::read_to_string(fixture.root.join("generated.txt")).unwrap(),
        "result"
    );
    assert!(!tools.commands_running());
    let review = tools.review().unwrap();
    assert_eq!(review.commands.last().unwrap().output, "firstsecond");
    assert!(
        review
            .changes
            .iter()
            .any(|change| change.path == "generated.txt")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn stop_command_and_host_teardown_kill_yielded_descendants() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let started = approved_session(
        &tools,
        "printf started; sleep 2; printf escaped > late.txt",
        0,
    )
    .await;
    let id = started["command_id"].as_str().unwrap();
    let stopped = tools.stop_command(id).await.unwrap();
    assert_eq!(stopped.status, "cancelled");
    assert!(!tools.commands_running());
    let second = approved_session(
        &tools,
        "printf started; sleep 2; printf escaped > late-again.txt",
        0,
    )
    .await;
    assert_eq!(second["status"], "running");
    // There is deliberately no explicit cancel here: workers must not retain
    // the last LocalTools owner through a cycle.
    drop(tools);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.root.join("late.txt").exists());
    assert!(!fixture.root.join("late-again.txt").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn command_sessions_are_owned_by_their_conversation_and_cancel_cleanly() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    if !tools.review().unwrap().sandbox_available {
        return;
    }
    let started = approved_session(&tools, "sleep 10", 0).await;
    let id = started["command_id"].as_str().unwrap();
    assert_eq!(
        other
            .tools(PermissionMode::WorkspaceWrite)
            .command_status(id, 0, 0)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    tools.cancel_commands_and_wait().await.unwrap();
    assert!(!tools.commands_running());
    assert!(!tools.review().unwrap().busy);
    assert_eq!(
        tools.command_status(id, 0, 0).await.unwrap().status,
        "cancelled"
    );
}
