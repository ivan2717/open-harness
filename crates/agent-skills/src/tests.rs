use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use agent_runtime::{ToolCall, ToolExecutor};

use crate::*;

mod namespaces;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "open-harness-skills-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn skill(&self, relative: &str, name: &str, body: &str) -> PathBuf {
        let directory = self.0.join(relative);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Helpful workflow\n---\n{body}\n"),
        )
        .unwrap();
        directory
    }

    fn config(&self) -> SkillCatalogConfig {
        SkillCatalogConfig {
            roots: vec![SkillRoot {
                path: self.0.clone(),
                scope: SkillScope::User,
                namespace: None,
            }],
            disabled_skill_ids: Vec::new(),
        }
    }

    fn catalog(&self) -> SkillCatalog {
        SkillCatalog::discover(self.config())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn project_skill_overrides_user_and_additional_independent_of_input_order() {
    let fixture = Fixture::new();
    fixture.skill("user/documents", "documents", "USER");
    fixture.skill(
        "project/.openharness/skills/documents",
        "documents",
        "PROJECT SECOND",
    );
    let winner = fixture.skill(
        "project/.agents/skills/documents",
        "documents",
        "PROJECT FIRST",
    );
    fixture.skill("other/documents", "documents", "ADDITIONAL");
    let mut config =
        SkillCatalogConfig::for_workspace(fixture.0.join("user"), Some(&fixture.0.join("project")));
    config.roots.push(SkillRoot {
        path: fixture.0.join("other"),
        scope: SkillScope::Additional,
        namespace: None,
    });
    config.roots.reverse();
    let catalog = SkillCatalog::discover(config);
    assert_eq!(catalog.skills().len(), 1);
    assert_eq!(catalog.skills()[0].scope, SkillScope::Project);
    assert_eq!(catalog.skills()[0].path, winner.join("SKILL.md"));
    assert!(
        catalog
            .load_skill("documents")
            .unwrap()
            .content
            .contains("PROJECT FIRST")
    );
    assert_eq!(
        catalog
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == "shadowed")
            .count(),
        3
    );
}

#[test]
fn malformed_entries_do_not_hide_valid_skills() {
    let fixture = Fixture::new();
    fixture.skill("good", "good", "Read the relevant file.");
    for (folder, text) in [
        ("missing", "# no frontmatter"),
        ("yaml", "---\nname: [\ndescription: nope\n---\nBody"),
        ("empty", "---\nname: empty\ndescription: ''\n---\nBody"),
        (
            "traversal",
            "---\nname: ../escape\ndescription: nope\n---\nBody",
        ),
        (
            "duplicate",
            "---\nname: first\nname: second\ndescription: nope\n---\nBody",
        ),
        ("no-body", "---\nname: no-body\ndescription: nope\n---\n"),
    ] {
        let directory = fixture.0.join(folder);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("SKILL.md"), text).unwrap();
    }
    let catalog = fixture.catalog();
    assert_eq!(catalog.skills().len(), 1);
    assert_eq!(catalog.skills()[0].id, "good");
    assert_eq!(catalog.diagnostics().len(), 6);
}

#[test]
fn unicode_folded_yaml_and_optional_metadata_are_supported() {
    let fixture = Fixture::new();
    let directory = fixture.skill("中文目录", "中文技能", "查看用户提供的材料。");
    fs::write(directory.join("SKILL.md"), "\u{feff}---\r\nname: 中文技能\r\ndescription: >-\r\n  阅读文档，\r\n  保留关键证据。\r\nmetadata:\r\n  short-description: 文档阅读\r\n---\r\n引用 resources/说明.md。\r\n").unwrap();
    fs::create_dir(directory.join("resources")).unwrap();
    fs::write(directory.join("resources/说明.md"), "证据：你好，世界。🌏").unwrap();
    let catalog = fixture.catalog();
    assert_eq!(catalog.skills()[0].description, "阅读文档， 保留关键证据。");
    assert!(
        catalog
            .load_skill("中文技能")
            .unwrap()
            .content
            .contains("引用 resources/说明.md")
    );
    assert_eq!(
        catalog
            .read_skill_resource("中文技能", "resources/说明.md")
            .unwrap()
            .content,
        "证据：你好，世界。🌏"
    );
}

#[test]
fn disabled_effective_skill_cannot_load_or_fall_back_to_lower_priority() {
    let fixture = Fixture::new();
    fixture.skill("user/docs", "docs", "USER");
    fixture.skill("project/.agents/skills/docs", "docs", "PROJECT");
    let mut config =
        SkillCatalogConfig::for_workspace(fixture.0.join("user"), Some(&fixture.0.join("project")));
    config.disabled_skill_ids.push("docs".to_owned());
    let catalog = SkillCatalog::discover(config);
    assert_eq!(catalog.skills().len(), 1);
    assert!(!catalog.skills()[0].enabled);
    assert!(
        catalog
            .load_skill("docs")
            .unwrap_err()
            .to_string()
            .contains("disabled")
    );
    assert!(catalog.read_skill_resource("docs", "SKILL.md").is_err());
    assert!(!catalog.prompt().contains("Helpful workflow"));
}

#[test]
fn prompt_and_list_are_metadata_only_and_load_returns_full_instructions() {
    let fixture = Fixture::new();
    fixture.skill("docs", "docs", "PRIVATE SKILL INSTRUCTION MARKER");
    let catalog = fixture.catalog();
    assert!(catalog.prompt().contains("Helpful workflow"));
    assert!(catalog.prompt().contains("$skill-name"));
    assert!(
        !catalog
            .prompt()
            .contains("PRIVATE SKILL INSTRUCTION MARKER")
    );
    assert!(
        catalog
            .load_skill("docs")
            .unwrap()
            .content
            .contains("PRIVATE SKILL INSTRUCTION MARKER")
    );
    assert!(
        !serde_json::to_string(catalog.skills())
            .unwrap()
            .contains("PRIVATE SKILL INSTRUCTION MARKER")
    );
}

#[test]
fn resources_are_bounded_utf8_text_and_never_execute_scripts() {
    let fixture = Fixture::new();
    let directory = fixture.skill("docs", "docs", "Read scripts/check.sh only.");
    fs::write(
        directory.join("large.md"),
        "a".repeat(MAX_RESOURCE_BYTES + 1),
    )
    .unwrap();
    fs::write(directory.join("binary.bin"), [0xff, 0x00]).unwrap();
    fs::write(directory.join("nul.txt"), "hello\0world").unwrap();
    fs::create_dir(directory.join("scripts")).unwrap();
    fs::write(directory.join("scripts/check.sh"), "touch SHOULD_NOT_EXIST").unwrap();
    let catalog = fixture.catalog();
    for path in ["large.md", "binary.bin", "nul.txt"] {
        assert!(catalog.read_skill_resource("docs", path).is_err());
    }
    assert_eq!(
        catalog
            .read_skill_resource("docs", "scripts/check.sh")
            .unwrap()
            .content,
        "touch SHOULD_NOT_EXIST"
    );
    assert!(!directory.join("SHOULD_NOT_EXIST").exists());
    fs::write(
        directory.join("boundary.txt"),
        "文".repeat(MAX_RESOURCE_BYTES / 3),
    )
    .unwrap();
    assert!(catalog.read_skill_resource("docs", "boundary.txt").is_ok());
}

#[test]
fn traversal_absolute_paths_and_secrets_are_denied() {
    let fixture = Fixture::new();
    fixture.skill("docs", "docs", "Read references.");
    let catalog = fixture.catalog();
    for path in [
        "../SKILL.md",
        "refs/../../outside.txt",
        "/etc/passwd",
        "C:\\secrets.txt",
        "",
        ".env",
        "references/.env.production",
        "server.key",
        "id_rsa",
        "references\0.txt",
    ] {
        assert!(
            catalog.read_skill_resource("docs", path).is_err(),
            "accepted {path:?}"
        );
    }
}

#[test]
fn snapshots_are_consistent_and_rediscovery_picks_up_changes() {
    let fixture = Fixture::new();
    fixture.skill("docs", "docs", "ORIGINAL BODY");
    let first = fixture.catalog();
    fixture.skill("docs", "docs", "UPDATED BODY");
    fixture.skill("new", "new", "NEW SKILL");
    assert_eq!(first.skills().len(), 1);
    assert!(
        first
            .load_skill("docs")
            .unwrap()
            .content
            .contains("ORIGINAL BODY")
    );
    assert_eq!(
        first
            .read_skill_resource("docs", "SKILL.md")
            .unwrap()
            .content,
        first.load_skill("docs").unwrap().content
    );
    let second = fixture.catalog();
    assert_eq!(second.skills().len(), 2);
    assert!(
        second
            .load_skill("docs")
            .unwrap()
            .content
            .contains("UPDATED BODY")
    );
    let mut config = fixture.config();
    config.disabled_skill_ids.push("docs".to_owned());
    assert!(SkillCatalog::discover(config).load_skill("docs").is_err());
}

#[test]
fn scan_limits_oversized_skills_and_missing_roots_are_reported() {
    let fixture = Fixture::new();
    fixture.skill("too-large", "too-large", &"x".repeat(MAX_SKILL_BYTES));
    fixture.skill("a/b/c/d/e/f/g", "too-deep", "Body");
    let mut config = fixture.config();
    config.roots.push(SkillRoot {
        path: fixture.0.join("missing"),
        scope: SkillScope::User,
        namespace: None,
    });
    let catalog = SkillCatalog::discover(config);
    assert!(catalog.skills().is_empty());
    assert_eq!(catalog.diagnostics().len(), 2);
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|item| item.code == "scan_limit")
    );
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|item| item.message.contains("byte limit"))
    );
}

#[cfg(unix)]
#[test]
fn escaping_symlinks_hardlinks_and_symlinked_roots_are_denied() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = Fixture::new();
    let skill = fixture.skill("docs", "docs", "Body");
    outside.skill("external", "external", "MUST NOT READ");
    fs::write(outside.0.join("secret.txt"), "PRIVATE CONTENT").unwrap();
    symlink(outside.0.join("external"), fixture.0.join("escape")).unwrap();
    symlink(&outside.0, skill.join("linked-dir")).unwrap();
    symlink(outside.0.join("secret.txt"), skill.join("linked-file")).unwrap();
    fs::hard_link(outside.0.join("secret.txt"), skill.join("hard-link")).unwrap();
    let catalog = fixture.catalog();
    assert_eq!(catalog.skills().len(), 1);
    for path in ["linked-file", "linked-dir/secret.txt", "hard-link"] {
        assert!(catalog.read_skill_resource("docs", path).is_err());
    }
    let linked_root = SkillCatalog::discover(SkillCatalogConfig {
        roots: vec![SkillRoot {
            path: fixture.0.join("escape"),
            scope: SkillScope::User,
            namespace: None,
        }],
        disabled_skill_ids: vec![],
    });
    assert!(linked_root.skills().is_empty());
    assert_eq!(linked_root.diagnostics().len(), 1);
    let directory_link = fixture.0.join(".agents");
    symlink(&outside.0, &directory_link).unwrap();
    let descendant = SkillCatalog::discover(SkillCatalogConfig {
        roots: vec![SkillRoot {
            path: directory_link.join("external"),
            scope: SkillScope::Project,
            namespace: None,
        }],
        disabled_skill_ids: vec![],
    });
    assert!(descendant.skills().is_empty());
    assert!(!descendant.diagnostics().is_empty());
}

#[cfg(unix)]
#[test]
fn replacing_directory_after_discovery_does_not_redirect_resource_reads() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = Fixture::new();
    let skill = fixture.skill("docs", "docs", "Body");
    fs::write(skill.join("reference.md"), "ORIGINAL RESOURCE").unwrap();
    fs::write(outside.0.join("reference.md"), "UNRELATED RESOURCE").unwrap();
    let catalog = fixture.catalog();
    fs::rename(&skill, fixture.0.join("moved")).unwrap();
    symlink(&outside.0, &skill).unwrap();
    assert_eq!(
        catalog
            .read_skill_resource("docs", "reference.md")
            .unwrap()
            .content,
        "ORIGINAL RESOURCE"
    );
}

#[tokio::test]
async fn tool_executor_loads_and_rejects_unknown_arguments() {
    let fixture = Fixture::new();
    fixture.skill("docs", "docs", "INSTRUCTION BODY");
    let catalog = fixture.catalog();
    assert_eq!(catalog.definitions().len(), 3);
    let execute = |name: &str, arguments: &str| {
        catalog.execute(ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: arguments.into(),
        })
    };
    let list = execute(LIST_SKILLS_TOOL, "{}").await.unwrap();
    assert!(list.contains("Helpful workflow"));
    assert!(!list.contains("INSTRUCTION BODY"));
    assert!(
        execute(LOAD_SKILL_TOOL, r#"{"id":"docs"}"#)
            .await
            .unwrap()
            .contains("INSTRUCTION BODY")
    );
    assert!(
        execute(
            READ_SKILL_RESOURCE_TOOL,
            r#"{"id":"docs","path":"SKILL.md"}"#
        )
        .await
        .is_ok()
    );
    assert!(
        execute(LIST_SKILLS_TOOL, r#"{"run":"something"}"#)
            .await
            .is_err()
    );
    assert!(
        execute(LOAD_SKILL_TOOL, r#"{"id":"docs","execute":true}"#)
            .await
            .is_err()
    );
    assert!(execute("execute_skill_script", "{}").await.is_err());
}

#[test]
fn supplied_roots_are_the_only_sources() {
    let config = SkillCatalogConfig::for_workspace(
        PathBuf::from("/app/data/skills"),
        Some(Path::new("/workspace")),
    );
    assert_eq!(config.roots.len(), 3);
    assert_eq!(config.roots[0].path, Path::new("/workspace/.agents/skills"));
    assert_eq!(
        config.roots[1].path,
        Path::new("/workspace/.openharness/skills")
    );
    assert_eq!(config.roots[2].path, Path::new("/app/data/skills"));
    assert!(
        config
            .roots
            .iter()
            .all(|root| !root.path.to_string_lossy().contains(".codex"))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_are_diagnosed_without_breaking_catalog_serialization() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let fixture = Fixture::new();
    fixture.skill("valid", "valid", "Keep this usable.");
    let directory = fixture.0.join(OsString::from_vec(vec![b's', 0xff]));
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        "---\nname: invalid-path\ndescription: Invalid directory bytes\n---\nInstructions.\n",
    )
    .unwrap();
    let catalog = fixture.catalog();
    assert_eq!(catalog.skills().len(), 1);
    assert_eq!(catalog.skills()[0].id, "valid");
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|item| item.code == "invalid_path")
    );
    assert!(serde_json::to_string(catalog.skills()).is_ok());
    assert!(serde_json::to_string(catalog.diagnostics()).is_ok());
}

#[cfg(unix)]
#[test]
fn non_utf8_root_diagnostics_are_serializable() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let fixture = Fixture::new();
    let catalog = SkillCatalog::discover(SkillCatalogConfig {
        roots: vec![SkillRoot {
            path: fixture.0.join(OsString::from_vec(vec![b's', 0xff])),
            scope: SkillScope::Additional,
            namespace: None,
        }],
        disabled_skill_ids: vec![],
    });
    assert!(catalog.skills().is_empty());
    assert_eq!(catalog.diagnostics()[0].code, "invalid_path");
    assert!(serde_json::to_string(catalog.diagnostics()).is_ok());
}
