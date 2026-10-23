use agent_local::{LocalTools, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "compact-tools-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        Self { base, root }
    }
    fn tools(&self, mode: PermissionMode) -> LocalTools {
        LocalTools::open(self.root.clone(), self.base.join("state"), mode).unwrap()
    }
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "compact-test".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}
async fn patch(tools: &LocalTools, body: &str) -> io::Result<String> {
    tools
        .execute(call(
            "apply_patch",
            json!({"patch":format!("*** Begin Patch\n{body}\n*** End Patch")}),
        ))
        .await
}

#[test]
fn advertises_only_the_compact_surface_available_to_the_permission_mode() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    let names: Vec<_> = tools
        .definitions()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        &names[..4],
        ["list_files", "read_file", "search_files", "apply_patch"]
    );
    assert!(
        names.len() == 4
            || names
                == [
                    "list_files",
                    "read_file",
                    "search_files",
                    "apply_patch",
                    "run_command"
                ]
    );
    let tools = fixture.tools(PermissionMode::ReadOnly);
    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["list_files", "read_file", "search_files"]
    );
    let schema: Value = serde_json::from_str(&tools.definitions()[1].input_schema).unwrap();
    assert!(schema["properties"]["cursor"].is_object());
    assert!(schema["properties"].get("offset").is_none());
}

#[tokio::test]
async fn patch_add_update_delete_survive_restart_and_undo_preserves_original_contents() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("edit.txt"), "user changes\nold\nend\n").unwrap();
    fs::write(fixture.root.join("delete.txt"), "delete me").unwrap();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    patch(&tools, "*** Add File: nested/new.txt\n+created\n*** Update File: edit.txt\n@@\n user changes\n-old\n+new\n end\n*** Delete File: delete.txt").await.unwrap();
    assert_eq!(fixture.read("nested/new.txt"), "created\n");
    assert_eq!(fixture.read("edit.txt"), "user changes\nnew\nend\n");
    assert!(!fixture.root.join("delete.txt").exists());
    assert_eq!(tools.review().unwrap().changes.len(), 3);
    drop(tools);
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for path in ["nested/new.txt", "edit.txt", "delete.txt"] {
        tools.revert(path).unwrap();
    }
    assert!(!fixture.root.join("nested/new.txt").exists());
    assert_eq!(fixture.read("edit.txt"), "user changes\nold\nend\n");
    assert_eq!(fixture.read("delete.txt"), "delete me");
    assert!(tools.review().unwrap().changes.is_empty());
}

#[tokio::test]
async fn ambiguous_stale_and_overlapping_hunks_are_rejected_without_partial_writes() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    fs::write(fixture.root.join("main.txt"), "old\nanchor\nold\n").unwrap();
    for body in [
        "*** Update File: main.txt\n@@\n-old\n+new",
        "*** Add File: would-exist.txt\n+new\n*** Update File: main.txt\n@@\n-stale\n+new",
        "*** Update File: main.txt\n@@\n-old\n+new\n anchor\n@@\n-anchor\n+changed\n old",
        "*** Add File: main.txt\n+new",
        "*** Delete File: missing.txt",
        "*** Update File: main.txt\n@@\n+ambiguous insertion",
    ] {
        assert!(patch(&tools, body).await.is_err(), "{body}");
        assert_eq!(fixture.read("main.txt"), "old\nanchor\nold\n");
        assert!(!fixture.root.join("would-exist.txt").exists());
        assert!(tools.review().unwrap().changes.is_empty());
    }
    patch(
        &tools,
        "*** Update File: main.txt\n@@\n-old\n+first\n anchor",
    )
    .await
    .unwrap();
    assert_eq!(fixture.read("main.txt"), "first\nanchor\nold\n");
}

#[tokio::test]
async fn patch_preserves_crlf_and_final_newline_and_supports_multiple_hunks() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for (name, before, after) in [
        (
            "crlf.txt",
            "one\r\nkeep\r\nthree\r\n",
            "ONE\r\nkeep\r\nTHREE\r\n",
        ),
        ("no-final.txt", "one\nkeep\nthree", "ONE\nkeep\nTHREE"),
        (
            "crlf-no-final.txt",
            "one\r\nkeep\r\nthree",
            "ONE\r\nkeep\r\nTHREE",
        ),
    ] {
        fs::write(fixture.root.join(name), before).unwrap();
        patch(
            &tools,
            &format!("*** Update File: {name}\n@@\n-one\n+ONE\n@@\n-three\n+THREE"),
        )
        .await
        .unwrap();
        assert_eq!(fixture.read(name), after);
        tools.revert(name).unwrap();
        assert_eq!(fixture.read(name), before);
    }
    fs::write(fixture.root.join("mixed.txt"), "a\r\nb\nc\n").unwrap();
    patch(&tools, "*** Update File: mixed.txt\n@@\n a\n-b\n+B\n c")
        .await
        .unwrap();
    assert_eq!(fixture.read("mixed.txt"), "a\r\nB\nc\n");
}

#[cfg(unix)]
#[tokio::test]
async fn deleted_file_permissions_survive_restart_undo_and_failed_batch_rollback() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    for (name, mode) in [("script.sh", 0o755), ("private.txt", 0o600)] {
        fs::write(fixture.root.join(name), "original\n").unwrap();
        fs::set_permissions(fixture.root.join(name), fs::Permissions::from_mode(mode)).unwrap();
    }
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    patch(
        &tools,
        "*** Delete File: script.sh\n*** Delete File: private.txt",
    )
    .await
    .unwrap();
    drop(tools);
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for (name, mode) in [("script.sh", 0o755), ("private.txt", 0o600)] {
        tools.revert(name).unwrap();
        assert_eq!(
            fs::metadata(fixture.root.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }
    fs::create_dir(fixture.root.join("locked")).unwrap();
    fs::set_permissions(
        fixture.root.join("locked"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    if fs::write(fixture.root.join("locked/probe"), "probe").is_ok() {
        return;
    }
    let result = patch(&tools, "*** Delete File: script.sh\n*** Delete File: private.txt\n*** Add File: locked/new.txt\n+blocked").await;
    fs::set_permissions(
        fixture.root.join("locked"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(result.is_err());
    for (name, mode) in [("script.sh", 0o755), ("private.txt", 0o600)] {
        assert_eq!(fixture.read(name), "original\n");
        assert_eq!(
            fs::metadata(fixture.root.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }
    assert!(tools.review().unwrap().changes.is_empty());
}

#[tokio::test]
async fn patch_rejects_readonly_traversal_protected_paths_and_aliases() {
    let fixture = Fixture::new();
    let readonly = fixture.tools(PermissionMode::ReadOnly);
    assert_eq!(
        patch(&readonly, "*** Add File: a\n+new")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    drop(readonly);
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    for path in [
        "../outside",
        "/tmp/outside",
        ".git/config",
        ".env",
        "nested/.ssh/key",
    ] {
        assert!(
            patch(&tools, &format!("*** Add File: {path}\n+new"))
                .await
                .is_err()
        );
    }
    for body in [
        "*** Add File: a\n+new\n*** Add File: ./a\n+other",
        "*** Add File: a\n+new\n*** Add File: a/b\n+other",
    ] {
        assert!(patch(&tools, body).await.is_err());
        assert!(!fixture.root.join("a").exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_multifile_write_rolls_back_prior_files_and_the_journal() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "before\n").unwrap();
    fs::create_dir(fixture.root.join("locked")).unwrap();
    fs::set_permissions(
        fixture.root.join("locked"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    // Root can bypass this fixture's permission failure.
    if fs::write(fixture.root.join("locked/probe"), "probe").is_ok() {
        return;
    }
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    let result = patch(
        &tools,
        "*** Update File: first.txt\n@@\n-before\n+after\n*** Add File: locked/new.txt\n+blocked",
    )
    .await;
    fs::set_permissions(
        fixture.root.join("locked"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(result.is_err());
    assert_eq!(fixture.read("first.txt"), "before\n");
    assert!(!fixture.root.join("locked/new.txt").exists());
    assert!(tools.review().unwrap().changes.is_empty());
}

#[tokio::test]
async fn patch_undo_refuses_intervening_user_changes() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    patch(&tools, "*** Add File: new.txt\n+agent")
        .await
        .unwrap();
    fs::write(fixture.root.join("new.txt"), "user work").unwrap();
    assert_eq!(
        tools.revert("new.txt").unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!tools.review().unwrap().changes[0].revertible);
    assert_eq!(fixture.read("new.txt"), "user work");
}

#[cfg(unix)]
#[tokio::test]
async fn patch_undo_preserves_intervening_user_permission_changes() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("file.txt"), "before\n").unwrap();
    fs::set_permissions(
        fixture.root.join("file.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    patch(&tools, "*** Update File: file.txt\n@@\n-before\n+after")
        .await
        .unwrap();
    fs::set_permissions(
        fixture.root.join("file.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    tools.revert("file.txt").unwrap();
    assert_eq!(fixture.read("file.txt"), "before\n");
    assert_eq!(
        fs::metadata(fixture.root.join("file.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn oversized_patch_response_is_rejected_before_any_mutation() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::WorkspaceWrite);
    let body = (0..64)
        .map(|index| format!("*** Add File: {}{index}\n+new", "\u{1}".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        patch(&tools, &body)
            .await
            .unwrap_err()
            .to_string()
            .contains("result exceeds")
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    assert!(tools.review().unwrap().changes.is_empty());
}

#[tokio::test]
async fn cursor_and_search_byte_budgets_paginate_without_losing_text_or_matches() {
    let fixture = Fixture::new();
    let tools = fixture.tools(PermissionMode::ReadOnly);
    let content = "汉字\n".repeat(6000);
    fs::write(fixture.root.join("text.txt"), &content).unwrap();
    let mut arguments = json!({"path":"text.txt"});
    let mut read = String::new();
    loop {
        let output = tools.execute(call("read_file", arguments)).await.unwrap();
        let page: Value = serde_json::from_str(&output).unwrap();
        read.push_str(page["content"].as_str().unwrap());
        if page["next_cursor"].is_null() {
            break;
        }
        arguments = json!({"path":"text.txt","cursor":page["next_cursor"]});
    }
    assert_eq!(read, content);
    for cursor in [
        json!({"unit":2,"offset":0}),
        json!({"unit":1,"offset":usize::MAX}),
    ] {
        assert!(
            tools
                .execute(call(
                    "read_file",
                    json!({"path":"text.txt","cursor":cursor})
                ))
                .await
                .is_err()
        );
    }
    let content = (0..120)
        .map(|index| format!("{}needle {index}\n", "\u{1}".repeat(200)))
        .collect::<String>();
    fs::write(fixture.root.join("escaped.txt"), content).unwrap();
    let mut offset = json!(0);
    let mut lines = Vec::new();
    loop {
        let output = tools
            .execute(call(
                "search_files",
                json!({"path":"escaped.txt","query":"needle","offset":offset}),
            ))
            .await
            .unwrap();
        assert!(output.len() <= 64 * 1024);
        let page: Value = serde_json::from_str(&output).unwrap();
        lines.extend(
            page["matches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["line"].as_u64().unwrap()),
        );
        offset = page["next_offset"].clone();
        if offset.is_null() {
            break;
        }
    }
    assert_eq!(lines, (1..=120).collect::<Vec<_>>());
}
