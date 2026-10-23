use super::*;
use agent_mcp::McpTransport;
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::json;
use std::sync::atomic::AtomicU64;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "harness-plugin-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn package(&self, version: &str, body: &str) -> PathBuf {
        let root = self.0.join("source");
        fs::create_dir_all(root.join(".codex-plugin")).unwrap();
        fs::create_dir_all(root.join("skills/review")).unwrap();
        fs::write(
            root.join(".codex-plugin/plugin.json"),
            json!({
                "name":"fixture-plugin", "version":version, "description":"Review documents",
                "skills":"./skills", "mcpServers":"./.mcp.json"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(".mcp.json"),
            json!({"mcpServers":{
                "fixture":{"command":"nonexistent-program-must-not-launch","args":[]}
            }})
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join("skills/review/SKILL.md"),
            format!("---\nname: review\ndescription: Review documents\n---\n{body}\n"),
        )
        .unwrap();
        root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "call".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

#[tokio::test]
async fn installed_components_are_namespaced_lazy_and_revoked_without_affecting_manual_extensions()
{
    let fixture = Fixture::new();
    let source = fixture.package("1.0.0", "PRIVATE_PLUGIN_BODY");
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    fs::create_dir_all(state.join("skills/review")).unwrap();
    fs::write(
        state.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Personal review\n---\nPERSONAL_BODY",
    )
    .unwrap();
    host.save(ExtensionConfig {
        mcp_servers: vec![McpServerConfig {
            id: "manual".into(),
            name: None,
            enabled: true,
            transport: McpTransport::Stdio {
                command: "nonexistent-manual".into(),
                args: vec![],
                env_vars: vec![],
            },
        }],
        ..Default::default()
    })
    .unwrap();
    let settings = host.install_plugin(&source, None).unwrap();
    assert_eq!(settings.plugins.len(), 1);
    assert_eq!(
        settings.config.mcp_servers.len(),
        1,
        "plugin components must not leak into manual config"
    );
    assert_eq!(settings.skills.len(), 2);
    assert!(
        settings
            .skills
            .iter()
            .any(|skill| skill.id == "fixture-plugin:review")
    );
    assert!(
        host.clients.read().unwrap().is_empty(),
        "installation must not connect"
    );

    let tools = ExtensionTools::new(host.clone(), "run".into(), None, None);
    let metadata = tools.execute(call("list_skills", json!({}))).await.unwrap();
    assert!(metadata.contains("fixture-plugin:review"));
    assert!(!metadata.contains("PRIVATE_PLUGIN_BODY"));
    assert!(
        tools
            .execute(call("load_skill", json!({"id":"fixture-plugin:review"})))
            .await
            .unwrap()
            .contains("PRIVATE_PLUGIN_BODY")
    );

    let server_id = settings.plugins[0].mcp_servers[0].id.clone();
    let old_config = host.server(&server_id).unwrap();
    let old_client = host.client("run", &old_config).unwrap();
    let pending_tools = ExtensionTools::new(host.clone(), "run".into(), None, None);
    let pending_id = server_id.clone();
    let task = tokio::spawn(async move {
        pending_tools
            .execute(call(
                "call_mcp_tool",
                json!({"server_id":pending_id,"name":"write","arguments":{}}),
            ))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while host.approvals.list("run").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    host.set_plugin_enabled("fixture-plugin", false).unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(host.approvals.list("run").is_empty());
    assert!(matches!(
        old_client.discover().await,
        Err(agent_mcp::McpError::Revoked)
    ));
    assert!(host.client("run", &old_config).is_err());
    assert!(
        tools
            .execute(call("load_skill", json!({"id":"fixture-plugin:review"})))
            .await
            .is_err()
    );
    assert!(
        tools
            .execute(call("load_skill", json!({"id":"review"})))
            .await
            .unwrap()
            .contains("PERSONAL_BODY")
    );
    assert_eq!(host.servers()[0].id, "manual");
    assert!(!ExtensionHost::open(&state).unwrap().settings().plugins[0].enabled);

    host.set_plugin_enabled("fixture-plugin", true).unwrap();
    assert_eq!(host.servers().len(), 2);
    host.uninstall_plugin("fixture-plugin").unwrap();
    assert!(host.settings().plugins.is_empty());
    assert_eq!(host.settings().skills.len(), 1);
    assert_eq!(host.servers().len(), 1);
    assert!(
        ExtensionHost::open(&state)
            .unwrap()
            .settings()
            .plugins
            .is_empty()
    );
}

#[tokio::test]
async fn plugin_updates_are_explicit_isolated_and_preserve_disabled_skills() {
    let fixture = Fixture::new();
    let source = fixture.package("1.0.0", "FIRST_BODY");
    let host = ExtensionHost::open(&fixture.0.join("state")).unwrap();
    let first = host.install_plugin(&source, None).unwrap();
    let old_hash = first.plugins[0].content_hash.clone();
    fixture.package("2.0.0", "SECOND_BODY");
    assert!(
        host.catalog(None)
            .load_skill("fixture-plugin:review")
            .unwrap()
            .content
            .contains("FIRST_BODY")
    );
    assert!(
        host.install_plugin(&source, None).is_err(),
        "duplicate import cannot implicitly overwrite"
    );
    assert!(host.install_plugin(&source, Some("different-id")).is_err());
    assert_eq!(host.settings().plugins[0].version, "1.0.0");
    let mut config = host.settings().config;
    config
        .disabled_skill_ids
        .push("fixture-plugin:review".into());
    host.save(config).unwrap();
    let updated = host
        .install_plugin(&source, Some("fixture-plugin"))
        .unwrap();
    assert_eq!(updated.plugins[0].version, "2.0.0");
    assert_ne!(updated.plugins[0].content_hash, old_hash);
    assert_eq!(
        updated.plugins[0].mcp_servers[0].id,
        first.plugins[0].mcp_servers[0].id
    );
    assert!(!updated.skills[0].enabled);
    let mut config = host.settings().config;
    config.disabled_skill_ids.clear();
    host.save(config).unwrap();
    assert!(
        host.catalog(None)
            .load_skill("fixture-plugin:review")
            .unwrap()
            .content
            .contains("SECOND_BODY")
    );
    fs::write(source.join(".codex-plugin/plugin.json"), "invalid json").unwrap();
    assert!(
        host.install_plugin(&source, Some("fixture-plugin"))
            .is_err()
    );
    assert_eq!(host.settings().plugins[0].version, "2.0.0");
}

#[test]
fn legacy_manual_plugin_prefix_is_preserved_and_actual_collisions_are_atomic() {
    let fixture = Fixture::new();
    let source = fixture.package("1.0.0", "BODY");
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    let settings = host.install_plugin(&source, None).unwrap();
    let mut manual = settings.plugins[0].mcp_servers[0].clone();
    let mut config = settings.config;
    config.mcp_servers.push(manual.clone());
    assert!(host.save(config).is_err());
    host.uninstall_plugin("fixture-plugin").unwrap();
    let mut config = host.settings().config;
    manual.name = Some("Existing manual integration".into());
    config.mcp_servers.push(manual.clone());
    host.save(config).unwrap();
    assert_eq!(
        ExtensionHost::open(&state).unwrap().servers()[0].id,
        manual.id
    );
    assert!(host.install_plugin(&source, None).is_err());
    assert!(host.settings().plugins.is_empty());
    assert!(
        ExtensionHost::open(&state)
            .unwrap()
            .settings()
            .plugins
            .is_empty()
    );
    assert_eq!(host.servers().len(), 1);
}

#[test]
fn plugin_install_and_enable_check_the_combined_skill_capacity() {
    let fixture = Fixture::new();
    let source = fixture.package("1.0.0", "BODY");
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    let add_skill = |index| {
        let name = format!("personal-{index}");
        let root = state.join("skills").join(&name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Personal workflow\n---\nInstructions.\n"),
        )
        .unwrap();
    };
    for index in 0..agent_skills::MAX_SKILLS {
        add_skill(index);
    }
    assert!(host.install_plugin(&source, None).is_err());
    assert!(host.settings().plugins.is_empty());
    fs::remove_dir_all(state.join("skills/personal-0")).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    assert_eq!(installed.skills.len(), agent_skills::MAX_SKILLS);
    host.set_plugin_enabled("fixture-plugin", false).unwrap();
    add_skill(0);
    assert!(host.set_plugin_enabled("fixture-plugin", true).is_err());
    assert!(!host.settings().plugins[0].enabled);
}
