//! Exercise the native sandbox used by scripts, with original files outside the
//! project. A host grant must not expose the containing attachment store.
#![cfg(target_os = "macos")]

use agent_local::{CommandStatus, LocalTools, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::json;
use std::{fs, path::PathBuf, time::Duration};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "openharness-attachment-sandbox-{}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("workspace")).unwrap();
        fs::create_dir_all(path.join("attachment-store")).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn quoted(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[tokio::test]
async fn original_attachment_read_is_exact_and_read_only_in_native_commands() {
    let fixture = Fixture::new();
    let store = fixture.0.join("attachment-store");
    // Production originals are content-addressed; no extension is required by
    // the command sandbox when the host identifies a specific original file.
    let original = store.join("a".repeat(64));
    let neighbour = store.join("b".repeat(64));
    fs::write(&original, "authorized original").unwrap();
    fs::write(&neighbour, "another conversation").unwrap();

    for (index, mode) in [PermissionMode::ReadOnly, PermissionMode::WorkspaceWrite]
        .into_iter()
        .enumerate()
    {
        let tools = LocalTools::open(
            fixture.0.join("workspace"),
            fixture.0.join(format!("state-{index}")),
            mode,
        )
        .unwrap();
        assert!(tools.review().unwrap().sandbox_available);
        tools.allow_attachment_read(&original).unwrap();
        let command = format!(
            "set -eu\n\
             test \"$(/bin/cat {original})\" = 'authorized original'\n\
             if /bin/cat {neighbour} >/dev/null 2>&1; then exit 31; fi\n\
             if /bin/ls {store} >/dev/null 2>&1; then exit 32; fi\n\
             if (printf mutation > {original}) 2>/dev/null; then exit 33; fi\n\
             if /bin/rm {original} >/dev/null 2>&1; then exit 34; fi\n\
             test \"$(/bin/cat {original})\" = 'authorized original'\n\
             printf 'exact attachment grant verified'",
            original = quoted(&original),
            neighbour = quoted(&neighbour),
            store = quoted(&store),
        );
        let worker = tools.clone();
        let task = tokio::spawn(async move {
            worker
                .execute(ToolCall {
                    id: "attachment-test".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":command,"yield_time_ms":10_000}).to_string(),
                })
                .await
        });
        let response = tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .expect("sandbox command did not finish")
            .unwrap()
            .unwrap();
        let status: CommandStatus = serde_json::from_str(&response).unwrap();
        assert_eq!(status.status, "completed", "{response}");
        assert_eq!(status.exit_code, Some(0), "{response}");
        assert_eq!(status.output, "exact attachment grant verified");
    }
    assert_eq!(
        fs::read_to_string(&original).unwrap(),
        "authorized original"
    );
    assert_eq!(
        fs::read_to_string(&neighbour).unwrap(),
        "another conversation"
    );
}
