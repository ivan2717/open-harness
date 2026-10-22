use super::*;
use agent_mcp::McpTransport;
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "harness-extensions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config() -> ExtensionConfig {
    ExtensionConfig {
        mcp_servers: vec![McpServerConfig {
            id: "fixture".into(),
            name: Some("Fixture".into()),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "nonexistent-program-must-not-launch".into(),
                args: vec![],
                env_vars: vec![],
            },
        }],
        ..Default::default()
    }
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}
fn skill(root: &Path, name: &str, body: &str) {
    fs::create_dir_all(root.join(name)).unwrap();
    fs::write(
        root.join(name).join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Fixture review skill\n---\n{body}"),
    )
    .unwrap();
}
async fn pending(host: &ExtensionHost, run: &str) -> ExtensionApproval {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(view) = host.approvals.list(run).pop() {
                return view;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn discovery_is_lazy_and_skills_are_refreshable_and_disableable() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    host.save(config()).unwrap(); // Does not execute the nonexistent program.
    skill(&dir.0.join("skills"), "review", "PRIVATE BODY");
    let tools = ExtensionTools::new(host.clone(), "run-a".into(), None, None);
    let list = tools.execute(call("list_skills", json!({}))).await.unwrap();
    assert!(list.contains("review"));
    assert!(!list.contains("PRIVATE BODY"));
    assert!(
        tools
            .execute(call("load_skill", json!({"id":"review"})))
            .await
            .unwrap()
            .contains("PRIVATE BODY")
    );
    skill(&dir.0.join("skills"), "new", "NEW BODY");
    assert!(!host.catalog(None).skills().iter().any(|s| s.id == "new"));
    assert!(
        tools
            .execute(call("list_skills", json!({})))
            .await
            .unwrap()
            .contains("new")
    );
    let mut disabled = config();
    disabled.disabled_skill_ids.push("review".into());
    disabled.revision = host.settings().config.revision;
    host.save(disabled).unwrap();
    assert!(
        tools
            .execute(call("load_skill", json!({"id":"review"})))
            .await
            .is_err()
    );
    let restored = ExtensionHost::open(&dir.0).unwrap();
    assert_eq!(
        restored.settings().config.disabled_skill_ids,
        vec!["review"]
    );
    assert_eq!(restored.servers().len(), 1);
}

#[tokio::test]
async fn denied_cancelled_and_stale_approvals_never_launch_servers() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    host.save(config()).unwrap();
    for action in ["deny", "cancel", "reconfigure"] {
        let tools = ExtensionTools::new(host.clone(), "run-a".into(), None, None);
        let task = tokio::spawn(async move {
            tools
                .execute(call(
                    "call_mcp_tool",
                    json!({"server_id":"fixture","name":"write","arguments":{"path":"a"}}),
                ))
                .await
        });
        let view = pending(&host, "run-a").await;
        assert_eq!(view.arguments["arguments"]["path"], "a");
        assert!(host.approvals.decide("run-b", &view.id, true).is_err());
        match action {
            "cancel" => task.abort(),
            "reconfigure" => {
                let mut updated = config();
                updated.mcp_servers[0].name = Some("Changed".into());
                updated.revision = host.settings().config.revision;
                host.save(updated).unwrap();
            }
            _ => host.approvals.decide("run-a", &view.id, false).unwrap(),
        }
        let result = task.await;
        if action != "cancel" {
            assert!(result.unwrap().unwrap_err().to_string().contains("denied"));
        }
        assert!(host.approvals.list("run-a").is_empty());
        assert!(host.approvals.decide("run-a", &view.id, true).is_err());
    }
}

#[tokio::test]
async fn readonly_composite_exposes_document_and_extension_tools_without_mutating_tools() {
    let dir = Directory::new();
    let project = dir.0.join("project");
    fs::create_dir_all(&project).unwrap();
    let local = agent_local::LocalTools::open(
        project.clone(),
        dir.0.join("journal"),
        agent_local::PermissionMode::ReadOnly,
    )
    .unwrap();
    let host = ExtensionHost::open(&dir.0.join("state")).unwrap();
    let tools = ExtensionTools::new(host, "run-a".into(), Some(project), Some(local));
    let names: Vec<_> = tools
        .definitions()
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    for name in [
        "read_document",
        "search_document",
        "load_skill",
        "call_mcp_tool",
    ] {
        assert!(names.contains(&name));
    }
    for name in ["run_command", "write_file", "edit_file"] {
        assert!(!names.contains(&name));
    }
    assert!(
        tools
            .execute(call("run_command", json!({"command":"touch blocked"})))
            .await
            .is_err()
    );
}

#[test]
fn invalid_extension_config_does_not_replace_the_saved_configuration() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    host.save(config()).unwrap();
    let mut duplicate = config();
    duplicate.mcp_servers.push(duplicate.mcp_servers[0].clone());
    assert!(host.save(duplicate).is_err());
    assert_eq!(ExtensionHost::open(&dir.0).unwrap().servers().len(), 1);
    assert!(
        host.save(config()).is_err(),
        "stale configuration must not overwrite a newer revision"
    );
    let mut large = config();
    if let McpTransport::Stdio { args, .. } = &mut large.mcp_servers[0].transport {
        *args = vec!["x".repeat(4096); 128];
    }
    assert!(host.save(large).is_err());
    assert_eq!(ExtensionHost::open(&dir.0).unwrap().servers().len(), 1);
}

#[test]
fn idle_clients_are_evicted_instead_of_exhausting_a_lifetime_quota() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    host.save(config()).unwrap();
    for index in 0..130 {
        host.client(&format!("run-{index}"), &host.server("fixture").unwrap())
            .unwrap();
    }
    assert_eq!(host.clients.read().unwrap().len(), 128);
    assert!(
        !host
            .clients
            .read()
            .unwrap()
            .contains_key(&("run-0".into(), "fixture".into()))
    );
}

#[tokio::test]
async fn unrelated_skill_changes_preserve_pending_mcp_approvals() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    host.save(config()).unwrap();
    let tools = ExtensionTools::new(host.clone(), "run-a".into(), None, None);
    let task = tokio::spawn(async move {
        tools
            .execute(call(
                "call_mcp_tool",
                json!({"server_id":"fixture","name":"write","arguments":{}}),
            ))
            .await
    });
    let view = pending(&host, "run-a").await;
    let mut updated = config();
    updated.disabled_skill_ids.push("review".into());
    updated.revision = host.settings().config.revision;
    host.save(updated).unwrap();
    assert_eq!(host.approvals.list("run-a")[0].id, view.id);
    host.approvals.decide("run-a", &view.id, false).unwrap();
    assert!(task.await.unwrap().is_err());
}
