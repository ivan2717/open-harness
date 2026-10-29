use agent_local::{LocalTools, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "openharness-file-boundaries-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("workspace")).unwrap();
        Self(fs::canonicalize(base).unwrap())
    }
    fn tools(&self) -> LocalTools {
        LocalTools::open(
            self.0.join("workspace"),
            self.0.join("state"),
            PermissionMode::WorkspaceWrite,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "file-boundary-test".into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

#[tokio::test]
async fn native_file_tools_refuse_hard_links_on_every_platform() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("outside.txt");
    fs::write(&outside, "outside-original").unwrap();
    fs::hard_link(&outside, fixture.0.join("workspace/alias.txt")).unwrap();
    let tools = fixture.tools();
    assert!(
        tools
            .execute(call("read_file", json!({"path":"alias.txt"})))
            .await
            .is_err()
    );
    assert!(tools.execute(call("write_file", json!({"path":"alias.txt","expected_content":"outside-original","content":"changed"}))).await.is_err());
    assert!(
        tools
            .execute(call(
                "apply_patch",
                json!({"patch":"*** Begin Patch\n*** Delete File: alias.txt\n*** End Patch"})
            ))
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside-original");
    assert!(fixture.0.join("workspace/alias.txt").is_file());
}

#[cfg(windows)]
#[tokio::test]
async fn native_file_tools_refuse_junction_ancestors_without_following_them() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("private.txt"), "private").unwrap();
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/j", "junction", r"..\outside"])
        .current_dir(fixture.0.join("workspace"))
        .status()
        .unwrap();
    assert!(status.success());
    let tools = fixture.tools();
    assert!(
        tools
            .execute(call("list_files", json!({"path":"junction"})))
            .await
            .is_err()
    );
    assert!(
        tools
            .execute(call("read_file", json!({"path":"junction/private.txt"})))
            .await
            .is_err()
    );
    assert!(
        tools
            .execute(call(
                "write_file",
                json!({"path":"junction/new.txt","expected_content":null,"content":"changed"})
            ))
            .await
            .is_err()
    );
    assert!(!outside.join("new.txt").exists());
    fs::remove_dir(fixture.0.join("workspace/junction")).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn native_file_tools_reject_alternate_stream_and_normalized_name_aliases() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("workspace/.env"), "secret").unwrap();
    let tools = fixture.tools();
    for path in [".env::$DATA", ".env.", ".env "] {
        assert!(
            tools
                .execute(call("read_file", json!({"path":path})))
                .await
                .is_err()
        );
    }
}

#[cfg(target_os = "macos")]
async fn command(tools: &LocalTools, script: &str) -> Value {
    let worker = tools.clone();
    let input = call(
        "run_command",
        json!({"command":script, "timeout_seconds":10,"access":"workspace_write"}),
    );
    let task = tokio::spawn(async move { worker.execute(input).await });
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !task.is_finished() {
            for approval in tools.review().unwrap().approvals {
                tools.approve(&approval.id, true).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        serde_json::from_str(&task.await.unwrap().unwrap()).unwrap()
    })
    .await
    .expect("command boundary test timed out")
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_sandbox_blocks_existing_hardlinks_parent_renames_and_new_hardlinks() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("outside.txt");
    fs::write(&outside, "outside-original").unwrap();
    fs::create_dir_all(fixture.0.join("workspace/links/nested")).unwrap();
    fs::hard_link(&outside, fixture.0.join("workspace/links/nested/alias.txt")).unwrap();
    fs::write(fixture.0.join("workspace/normal.txt"), "NORMAL_OK").unwrap();
    let result = command(&fixture.tools(), "if cat links/nested/alias.txt >/dev/null 2>&1; then exit 11; fi\nif printf changed > links/nested/alias.txt 2>/dev/null; then exit 12; fi\nif mv links moved 2>/dev/null; then exit 13; fi\nif mv links/nested nested-moved 2>/dev/null; then exit 14; fi\nif ln normal.txt new-link.txt 2>/dev/null; then exit 15; fi\ncat normal.txt\nprintf ordinary > ordinary.txt").await;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(result["output"].as_str().unwrap().contains("NORMAL_OK"));
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside-original");
    assert_eq!(
        fs::read_to_string(fixture.0.join("workspace/ordinary.txt")).unwrap(),
        "ordinary"
    );
    assert!(!fixture.0.join("workspace/new-link.txt").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_sandbox_preserves_internal_dependency_symlinks_but_denies_external_targets() {
    let fixture = Fixture::new();
    let root = fixture.0.join("workspace");
    fs::create_dir_all(root.join("node_modules/.pnpm/pkg")).unwrap();
    fs::write(
        root.join("node_modules/.pnpm/pkg/value.txt"),
        "DEPENDENCY_OK",
    )
    .unwrap();
    std::os::unix::fs::symlink(".pnpm/pkg", root.join("node_modules/pkg")).unwrap();
    fs::write(fixture.0.join("outside.txt"), "outside-original").unwrap();
    std::os::unix::fs::symlink(fixture.0.join("outside.txt"), root.join("external.txt")).unwrap();
    let result = command(&fixture.tools(), "set -e\ncat node_modules/pkg/value.txt\nif cat external.txt >/dev/null 2>&1; then exit 21; fi\nif printf changed > external.txt 2>/dev/null; then exit 22; fi").await;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(result["output"].as_str().unwrap().contains("DEPENDENCY_OK"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("outside.txt")).unwrap(),
        "outside-original"
    );
}
