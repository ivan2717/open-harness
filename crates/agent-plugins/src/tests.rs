use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use agent_mcp::McpTransport;
use serde_json::json;

use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(unique_name("agent-plugins-test"));
        fs::create_dir(&root).unwrap();
        Self {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn source(&self, id: &str, version: &str) -> PathBuf {
        let root = self.root.join(format!("source-{id}"));
        fs::create_dir_all(root.join("skills/review")).unwrap();
        fs::write(
            root.join("plugin.json"),
            json!({"name":id,"version":version,"description":"Review documents"}).to_string(),
        )
        .unwrap();
        fs::write(root.join("skills/review/SKILL.md"), "---\nname: review\ndescription: Review a document\n---\nRead the document and report concrete findings.\n").unwrap();
        root
    }
    fn store(&self) -> PluginStore {
        PluginStore::open(&self.root.join("store")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut archive = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, content) in entries {
        archive
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(content).unwrap();
    }
    archive.finish().unwrap();
}

#[test]
fn lifecycle_preserves_namespaces_disabled_state_and_inflight_snapshots() {
    let fixture = Fixture::new();
    let source = fixture.source("sample.plugin", "1.0.0");
    let mut store = fixture.store();
    let first = store.install(&source, None).unwrap();
    assert_eq!(first.skill_count, 1);
    assert_eq!(first.content_hash.len(), 64);
    assert!(first.enabled);
    let roots = store.skill_roots();
    assert_eq!(roots[0].0, "sample.plugin");
    assert!(roots[0].1.join("review/SKILL.md").is_file());
    assert!(store.install(&source, None).is_err());
    store.set_enabled("sample.plugin", false).unwrap();
    assert!(store.skill_roots().is_empty());
    fs::write(
        source.join("plugin.json"),
        json!({"name":"sample.plugin","version":"2.0.0"}).to_string(),
    )
    .unwrap();
    let updated = store.install(&source, Some("sample.plugin")).unwrap();
    assert!(!updated.enabled);
    assert_ne!(updated.content_hash, first.content_hash);
    store.set_enabled("sample.plugin", true).unwrap();
    assert_ne!(roots[0].1, store.skill_roots()[0].1);
    assert!(
        roots[0].1.is_dir(),
        "an active run can still read its old revision"
    );
    store.uninstall("sample.plugin").unwrap();
    assert!(store.list().is_empty());
    drop(store);
    assert!(fixture.store().list().is_empty());
    assert!(
        !roots[0].1.exists(),
        "next startup reclaims unreferenced revisions"
    );
}

#[test]
fn update_validation_and_host_guard_leave_old_installation_intact() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    let mut store = fixture.store();
    let original = store.install(&source, None).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":"different","version":"2"}).to_string(),
    )
    .unwrap();
    assert!(store.install(&source, Some("sample")).is_err());
    fs::write(
        source.join("plugin.json"),
        json!({"name":"sample","version":"2"}).to_string(),
    )
    .unwrap();
    assert!(
        store
            .install_checked(&source, Some("sample"), |_| Err(invalid("host collision")))
            .is_err()
    );
    assert_eq!(store.list()[0].content_hash, original.content_hash);
    assert_eq!(fixture.store().list()[0].version, "1");
    assert_eq!(
        fs::read_dir(fixture.root.join("store/staging"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn failed_atomic_registry_publish_keeps_old_state() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    let mut store = fixture.store();
    store.install(&source, None).unwrap();
    let registry = fixture.root.join("store/registry.json");
    let saved = fs::read(&registry).unwrap();
    fs::remove_file(&registry).unwrap();
    fs::create_dir(&registry).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":"sample","version":"2"}).to_string(),
    )
    .unwrap();
    assert!(store.install(&source, Some("sample")).is_err());
    assert_eq!(store.list()[0].version, "1");
    assert!(store.set_enabled("sample", false).is_err());
    assert!(store.list()[0].enabled);
    fs::remove_dir(&registry).unwrap();
    fs::write(&registry, saved).unwrap();
    assert_eq!(fixture.store().list()[0].version, "1");
}

#[test]
fn legacy_mcp_import_namespaces_ids_and_never_starts_a_process() {
    let fixture = Fixture::new();
    let source = fixture.source("legacy", "1");
    fs::remove_file(source.join("plugin.json")).unwrap();
    fs::create_dir(source.join(".codex-plugin")).unwrap();
    fs::write(source.join(".codex-plugin/plugin.json"), json!({"name":"legacy","version":"1","skills":"./skills","mcpServers":"./.mcp.json","hooks":{}}).to_string()).unwrap();
    fs::write(source.join(".mcp.json"), json!({"mcpServers":{"fixture":{"command":"nonexistent-program-must-not-launch","args":[],"env":{"API_TOKEN":"${API_TOKEN}"}}}}).to_string()).unwrap();
    let mut store = fixture.store();
    let info = store.install(&source, None).unwrap();
    assert!(
        info.warnings
            .iter()
            .any(|warning| warning.contains("hooks"))
    );
    let first = store.servers()[0].clone();
    assert!(first.id.starts_with("plugin_"));
    assert!(first.id.len() <= 64);
    assert_eq!(first.name.as_deref(), Some("legacy · fixture"));
    assert!(
        matches!(&first.transport, McpTransport::Stdio { env_vars, .. } if env_vars == &["API_TOKEN"])
    );
    let again = store.install(&source, Some("legacy")).unwrap();
    assert_eq!(again.mcp_servers[0].id, first.id);
    store.set_enabled("legacy", false).unwrap();
    assert!(store.servers().is_empty());
}

#[test]
fn http_auth_references_are_supported_but_secret_literals_are_rejected() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    fs::write(source.join("mcp.json"), json!({"mcpServers":{"remote":{"url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${MCP_TOKEN}"}}}}).to_string()).unwrap();
    let mut store = fixture.store();
    let info = store.install(&source, None).unwrap();
    assert!(
        matches!(&info.mcp_servers[0].transport, McpTransport::Http { bearer_token_env: Some(name), .. } if name == "MCP_TOKEN")
    );
    for config in [
        json!({"command":"node","env":{"TOKEN":"sensitive-value"}}),
        json!({"url":"https://example.com","headers":{"Authorization":"Bearer sensitive-value"}}),
        json!({"url":"https://example.com","oauth":{}}),
    ] {
        fs::write(
            source.join("mcp.json"),
            json!({"mcpServers":{"remote":config}}).to_string(),
        )
        .unwrap();
        let error = store
            .install(&source, Some("sample"))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("sensitive-value"));
        assert_eq!(store.list()[0].content_hash, info.content_hash);
    }
}

#[test]
fn bundled_mcp_paths_resolve_to_the_immutable_installation() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    fs::create_dir(source.join("bin")).unwrap();
    fs::write(source.join("bin/server"), "#!/bin/sh\nexit 99\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("bin/server"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(source.join("mcp.json"), json!({"mcpServers":{"local":{"command":"./bin/server","args":["${CODEX_PLUGIN_ROOT}","--config=${CLAUDE_PLUGIN_ROOT}/plugin.json"]}}}).to_string()).unwrap();
    let mut store = fixture.store();
    let info = store.install(&source, None).unwrap();
    let McpTransport::Stdio { command, args, .. } = &info.mcp_servers[0].transport else {
        panic!()
    };
    assert!(Path::new(command).is_file());
    assert!(command.starts_with(fixture.root.join("store/packages").to_str().unwrap()));
    assert!(!command.contains("${"));
    assert!(args.iter().all(|arg| !arg.contains("${")));
}

#[test]
fn explicit_relative_mcp_arguments_are_resolved_and_other_arguments_are_literal() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    fs::write(
        source.join("server.js"),
        "throw new Error('must not run during install');",
    )
    .unwrap();
    fs::write(
        source.join("mcp.json"),
        json!({"mcpServers":{"local":{"command":"node","args":["./server.js"]}}}).to_string(),
    )
    .unwrap();
    let mut store = fixture.store();
    let info = store.install(&source, None).unwrap();
    let McpTransport::Stdio { args, .. } = &info.mcp_servers[0].transport else {
        panic!()
    };
    assert!(Path::new(&args[0]).is_file());
    assert!(Path::new(&args[0]).is_absolute());
    fs::write(
        source.join("mcp.json"),
        json!({"mcpServers":{"local":{"command":"node","args":["server.js"]}}}).to_string(),
    )
    .unwrap();
    let updated = store.install(&source, Some("sample")).unwrap();
    let McpTransport::Stdio { args, .. } = &updated.mcp_servers[0].transport else {
        panic!()
    };
    assert_eq!(args, &["server.js"]);
}

#[cfg(unix)]
#[test]
fn execution_permission_changes_are_detected_on_restart() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    fs::write(source.join("script.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(source.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut store = fixture.store();
    store.install(&source, None).unwrap();
    let package = store.skill_roots()[0].1.parent().unwrap().to_path_buf();
    fs::set_permissions(package.join("script.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    drop(store);
    assert!(!fixture.store().list()[0].enabled);
}

#[cfg(unix)]
#[test]
fn nonexecutable_bundled_command_fails_with_actionable_error() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    fs::write(source.join("server.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(source.join("server.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(
        source.join("mcp.json"),
        json!({"mcpServers":{"local":{"command":"./server.sh"}}}).to_string(),
    )
    .unwrap();
    let error = fixture
        .store()
        .install(&source, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("interpreter"));
}

#[test]
fn zip_import_accepts_root_and_single_wrapper_packages() {
    let fixture = Fixture::new();
    let manifest = br#"{"name":"zip-plugin","version":"1","mcpServers":{"example":{"command":"not-a-program"}}}"#;
    for prefix in ["", "zip-plugin/"] {
        let zip = fixture.root.join("package.zip");
        write_zip(&zip, &[(format!("{prefix}plugin.json").as_str(), manifest)]);
        let mut store = fixture.store();
        let info = store.install(&zip, None).unwrap();
        assert_eq!(info.id, "zip-plugin");
        store.uninstall("zip-plugin").unwrap();
    }
}

#[test]
fn zip_traversal_duplicate_paths_and_expansion_limits_are_rejected() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    for name in ["../outside", "/absolute", "C:/absolute", "folder\\escape"] {
        let zip = fixture.root.join("bad.zip");
        write_zip(&zip, &[(name, b"bad")]);
        assert!(store.install(&zip, None).is_err(), "{name}");
        assert!(store.list().is_empty());
    }
    let zip = fixture.root.join("duplicate.zip");
    write_zip(&zip, &[("plugin.json", b"one"), ("Plugin.json", b"two")]);
    assert!(store.install(&zip, None).is_err());
    let zip = fixture.root.join("oversize.zip");
    write_zip(
        &zip,
        &[("huge.txt", &vec![0; package::MAX_FILE_BYTES as usize + 1])],
    );
    assert!(store.install(&zip, None).is_err());
    assert!(!fixture.root.join("outside").exists());
}

#[cfg(unix)]
#[test]
fn directory_symlinks_hardlinks_and_manifest_escape_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    let outside = fixture.root.join("private.txt");
    fs::write(&outside, "private").unwrap();
    let mut store = fixture.store();
    symlink(&outside, source.join("link")).unwrap();
    assert!(store.install(&source, None).is_err());
    fs::remove_file(source.join("link")).unwrap();
    fs::hard_link(&outside, source.join("hardlink")).unwrap();
    assert!(store.install(&source, None).is_err());
    fs::remove_file(source.join("hardlink")).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":"sample","skills":"../outside"}).to_string(),
    )
    .unwrap();
    assert!(store.install(&source, None).is_err());
    assert!(store.list().is_empty());
}

#[test]
fn malformed_skill_and_unsupported_only_packages_are_rejected() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    let mut store = fixture.store();
    fs::write(source.join("skills/review/SKILL.md"), "not a skill").unwrap();
    assert!(store.install(&source, None).is_err());
    fs::remove_dir_all(source.join("skills")).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":"sample","apps":{},"hooks":{}}).to_string(),
    )
    .unwrap();
    let error = store.install(&source, None).unwrap_err().to_string();
    assert!(error.contains("no supported skills or MCP"));
    assert!(error.contains("hooks"));
}

#[test]
fn damaged_package_does_not_prevent_startup_or_uninstall() {
    let fixture = Fixture::new();
    let source = fixture.source("sample", "1");
    let mut store = fixture.store();
    store.install(&source, None).unwrap();
    let root = store.skill_roots()[0].1.clone();
    fs::write(root.join("review/SKILL.md"), "modified").unwrap();
    drop(store);
    let mut reopened = fixture.store();
    assert!(!reopened.list()[0].enabled);
    assert!(
        reopened.list()[0]
            .warnings
            .iter()
            .any(|s| s.contains("unsafe"))
    );
    assert!(reopened.set_enabled("sample", true).is_err());
    assert!(reopened.servers().is_empty());
    assert!(reopened.skill_roots().is_empty());
    reopened.uninstall("sample").unwrap();
    assert!(reopened.list().is_empty());
}

#[test]
fn capabilities_have_global_limits_even_when_disabled() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let source = fixture.source("first", "1");
    let servers = (0..64)
        .map(|i| (format!("server{i}"), json!({"command":"unused"})))
        .collect::<serde_json::Map<_, _>>();
    fs::write(
        source.join("mcp.json"),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
    store.install(&source, None).unwrap();
    store.set_enabled("first", false).unwrap();
    let other = fixture.source("second", "1");
    fs::write(
        other.join("mcp.json"),
        json!({"mcpServers":{"one":{"command":"unused"}}}).to_string(),
    )
    .unwrap();
    assert!(store.install(&other, None).is_err());
    assert_eq!(store.list().len(), 1);
}
