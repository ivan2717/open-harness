use agent_local::{LocalTools, PermissionMode, WorkspaceAccess};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    access: WorkspaceAccess,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir().join(format!(
            "agent-collaboration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let access = WorkspaceAccess::new(root.clone()).unwrap();
        Self { base, root, access }
    }

    fn tools(&self, owner: &str, allowed: Option<&[&str]>, mode: PermissionMode) -> LocalTools {
        LocalTools::open(self.root.clone(), self.base.join(owner), mode)
            .unwrap()
            .with_collaboration(self.access.clone(), owner, allowed.map(strings))
            .unwrap()
    }

    fn text(&self, path: &str) -> String {
        fs::read_to_string(self.root.join(path)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).into()).collect()
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "test".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}
fn write(path: &str, expected: Option<&str>, content: &str) -> ToolCall {
    call(
        "write_file",
        json!({"path":path,"expected_content":expected,"content":content}),
    )
}

#[tokio::test]
async fn delegates_write_only_exact_leased_files_and_parent_respects_reservations() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a.txt"), "a").unwrap();
    let parent = fixture.tools("parent", None, PermissionMode::WorkspaceWrite);
    let child = fixture.tools("child", Some(&["./a.txt"]), PermissionMode::WorkspaceWrite);
    assert!(
        child
            .execute(write("a.txt", Some("a"), "child"))
            .await
            .is_err()
    );
    fixture
        .access
        .acquire("child", &strings(&["a.txt"]))
        .await
        .unwrap();
    child
        .execute(call(
            "edit_file",
            json!({"path":"./a.txt","old_text":"a","new_text":"child"}),
        ))
        .await
        .unwrap();
    assert_eq!(fixture.text("a.txt"), "child");
    assert_eq!(child.review().unwrap().changes[0].path, "a.txt");
    assert!(
        parent
            .execute(write("./a.txt", Some("child"), "parent"))
            .await
            .is_err()
    );
    assert!(
        child
            .execute(write("b.txt", None, "outside"))
            .await
            .is_err()
    );
    parent
        .execute(write("b.txt", None, "parent"))
        .await
        .unwrap();
    assert_eq!(fixture.text("b.txt"), "parent");
    fixture.access.release("child").await;
    assert!(
        child
            .execute(write("a.txt", Some("child"), "late child"))
            .await
            .is_err()
    );
    parent
        .execute(write("a.txt", Some("child"), "parent"))
        .await
        .unwrap();
    assert_eq!(fixture.text("a.txt"), "parent");
}

#[tokio::test]
async fn unauthorized_patch_never_changes_any_file_or_journal() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    fs::write(fixture.root.join("b.txt"), "b\n").unwrap();
    let child = fixture.tools("child", Some(&["a.txt"]), PermissionMode::WorkspaceWrite);
    fixture
        .access
        .acquire("child", &strings(&["a.txt"]))
        .await
        .unwrap();
    let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+changed\n*** Delete File: b.txt\n*** End Patch";
    assert_eq!(
        child
            .execute(call("apply_patch", json!({"patch":patch})))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fixture.text("a.txt"), "a\n");
    assert_eq!(fixture.text("b.txt"), "b\n");
    assert!(child.review().unwrap().changes.is_empty());
    let patch = "*** Begin Patch\n*** Update File: ./a.txt\n@@\n-a\n+changed\n*** End Patch";
    child
        .execute(call("apply_patch", json!({"patch":patch})))
        .await
        .unwrap();
    assert_eq!(fixture.text("a.txt"), "changed\n");
    child.revert("./a.txt").unwrap();
    assert_eq!(fixture.text("a.txt"), "a\n");
}

#[tokio::test]
async fn normalized_leases_reject_aliases_overlaps_and_protected_paths_atomically() {
    let fixture = Fixture::new();
    for path in [
        "",
        ".",
        "..",
        "a/../b",
        "/outside",
        ".git/config",
        ".env",
        "a/.ssh/key",
    ] {
        assert!(
            fixture
                .access
                .acquire("bad", &strings(&[path]))
                .await
                .is_err(),
            "{path}"
        );
    }
    fs::create_dir(fixture.root.join("directory")).unwrap();
    assert!(
        fixture
            .access
            .acquire("bad", &strings(&["directory"]))
            .await
            .is_err()
    );
    fixture
        .access
        .acquire("one", &strings(&["./a.txt", "a.txt", "future"]))
        .await
        .unwrap();
    fixture
        .access
        .acquire("one", &strings(&["future", "a.txt"]))
        .await
        .unwrap();
    assert!(
        fixture
            .access
            .acquire("two", &strings(&["A.TXT"]))
            .await
            .is_err()
    );
    assert!(
        fixture
            .access
            .acquire("two", &strings(&["future/child.txt"]))
            .await
            .is_err()
    );
    assert!(
        fixture
            .access
            .acquire("two", &strings(&["free.txt", "a.txt"]))
            .await
            .is_err()
    );
    // A failed multi-file lease must not retain its nonconflicting prefix.
    fixture
        .access
        .acquire("three", &strings(&["free.txt"]))
        .await
        .unwrap();
    let race_paths = strings(&["race.txt"]);
    let (first, second) = tokio::join!(
        fixture.access.acquire("four", &race_paths),
        fixture.access.acquire("five", &race_paths)
    );
    assert_ne!(first.is_ok(), second.is_ok());
}

#[tokio::test]
async fn non_waiting_leases_enforce_the_same_path_and_conflict_rules() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .access
            .try_acquire("bad", &strings(&["../outside"]))
            .is_err()
    );
    fixture
        .access
        .try_acquire("one", &strings(&["./a.txt"]))
        .unwrap();
    fixture
        .access
        .try_acquire("one", &strings(&["a.txt"]))
        .unwrap();
    assert_eq!(
        fixture
            .access
            .try_acquire("two", &strings(&["free.txt", "a.txt"]))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    fixture
        .access
        .try_acquire("three", &strings(&["free.txt"]))
        .unwrap();
    fixture.access.release("one").await;
    fixture
        .access
        .try_acquire("two", &strings(&["a.txt"]))
        .unwrap();
}

#[tokio::test]
async fn unicode_filename_aliases_cannot_bypass_a_write_reservation() {
    let fixture = Fixture::new();
    let composed = "café.txt";
    let decomposed = "cafe\u{301}.txt";
    fs::write(fixture.root.join(composed), "original").unwrap();
    let parent = fixture.tools("parent", None, PermissionMode::WorkspaceWrite);
    fixture
        .access
        .try_acquire("child", &strings(&[composed]))
        .unwrap();
    assert!(
        fixture
            .access
            .try_acquire("other", &strings(&[decomposed]))
            .is_err()
    );
    let error = parent
        .execute(write(decomposed, Some("original"), "unsafe"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reserved by another"));
    assert_eq!(fixture.text(composed), "original");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_lease_and_replaced_symlink_target_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a.txt"), "original").unwrap();
    symlink("a.txt", fixture.root.join("alias.txt")).unwrap();
    assert!(
        fixture
            .access
            .acquire("child", &strings(&["alias.txt"]))
            .await
            .is_err()
    );
    let child = fixture.tools("child", Some(&["a.txt"]), PermissionMode::WorkspaceWrite);
    fixture
        .access
        .acquire("child", &strings(&["a.txt"]))
        .await
        .unwrap();
    fs::write(fixture.base.join("outside.txt"), "outside").unwrap();
    fs::remove_file(fixture.root.join("a.txt")).unwrap();
    symlink(fixture.base.join("outside.txt"), fixture.root.join("a.txt")).unwrap();
    assert!(
        child
            .execute(write("a.txt", Some("outside"), "unsafe"))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.base.join("outside.txt")).unwrap(),
        "outside"
    );
}

#[tokio::test]
async fn restrictions_cover_legacy_tools_revert_and_original_permission_mode() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", None, PermissionMode::WorkspaceWrite);
    parent
        .execute(write("a.txt", None, "parent"))
        .await
        .unwrap();
    let child = fixture.tools("child", Some(&["a.txt"]), PermissionMode::ReadOnly);
    fixture
        .access
        .acquire("child", &strings(&["a.txt"]))
        .await
        .unwrap();
    assert!(
        child
            .execute(write("a.txt", Some("parent"), "unsafe"))
            .await
            .is_err()
    );
    assert!(
        child
            .execute(call(
                "edit_file",
                json!({"path":"a.txt","old_text":"parent","new_text":"unsafe"})
            ))
            .await
            .is_err()
    );
    assert!(child.revert("a.txt").is_err());
    assert!(parent.revert("./a.txt").is_err());
    fixture.access.release("child").await;
    parent.revert("./a.txt").unwrap();
    assert!(!fixture.root.join("a.txt").exists());
    let reader = fixture.tools("reader", Some(&[]), PermissionMode::WorkspaceWrite);
    assert!(
        reader
            .definitions()
            .iter()
            .all(|tool| !matches!(tool.name.as_str(), "apply_patch" | "run_command"))
    );
    assert!(
        reader
            .execute(write("new.txt", None, "unsafe"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn commands_are_never_allowed_for_delegates_or_while_a_write_lease_exists() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", None, PermissionMode::WorkspaceWrite);
    let child = fixture.tools("child", Some(&["a.txt"]), PermissionMode::WorkspaceWrite);
    assert!(
        child
            .definitions()
            .iter()
            .all(|tool| tool.name != "run_command")
    );
    assert!(
        child
            .execute(call("run_command", json!({"command":"echo unsafe"})))
            .await
            .unwrap_err()
            .to_string()
            .contains("delegated runs")
    );
    fixture
        .access
        .acquire("child", &strings(&["a.txt"]))
        .await
        .unwrap();
    assert!(
        parent
            .execute(call("run_command", json!({"command":"echo unsafe"})))
            .await
            .unwrap_err()
            .to_string()
            .contains("delegated file writes")
    );
    assert!(parent.review().unwrap().approvals.is_empty());
    assert!(parent.review().unwrap().commands.is_empty());
}

#[tokio::test]
async fn cancelling_a_waiting_lease_and_command_releases_coordination_without_deadlock() {
    let fixture = Fixture::new();
    let parent = fixture.tools("parent", None, PermissionMode::WorkspaceWrite);
    if !parent.review().unwrap().sandbox_available {
        return;
    }
    let runner = parent.clone();
    let command = tokio::spawn(async move {
        runner
            .execute(call("run_command", json!({"command":"echo not-executed"})))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !parent.review().unwrap().approvals.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture
            .access
            .try_acquire("non-waiting-child", &strings(&["a.txt"]))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let access = fixture.access.clone();
    let lease = tokio::spawn(async move {
        access
            .acquire("cancelled-child", &strings(&["a.txt"]))
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !lease.is_finished(),
        "a command must retain its workspace operation lock while awaiting approval"
    );
    lease.abort();
    assert!(lease.await.unwrap_err().is_cancelled());
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    assert!(parent.review().unwrap().approvals.is_empty());
    tokio::time::timeout(
        Duration::from_secs(2),
        fixture.access.acquire("next-child", &strings(&["a.txt"])),
    )
    .await
    .unwrap()
    .unwrap();
    let child = fixture.tools(
        "next-child",
        Some(&["a.txt"]),
        PermissionMode::WorkspaceWrite,
    );
    child
        .execute(write("a.txt", None, "child result"))
        .await
        .unwrap();
    fixture.access.release("next-child").await;
    assert!(
        child
            .execute(write("a.txt", Some("child result"), "late mutation"))
            .await
            .is_err()
    );
    parent
        .execute(write("a.txt", Some("child result"), "parent result"))
        .await
        .unwrap();
    assert_eq!(fixture.text("a.txt"), "parent result");
}
