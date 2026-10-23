use super::*;

fn plugin_root(fixture: &Fixture, relative: &str, plugin_id: &str) -> SkillRoot {
    SkillRoot {
        path: fixture.0.join(relative),
        scope: SkillScope::Additional,
        namespace: Some(plugin_id.to_owned()),
    }
}

#[test]
fn plugin_namespaces_coexist_with_standalone_scope_overrides() {
    let fixture = Fixture::new();
    fixture.skill("user/review", "review", "USER");
    fixture.skill("project/.agents/skills/review", "review", "PROJECT");
    fixture.skill("alpha/review", "review", "ALPHA");
    fixture.skill("beta/review", "review", "BETA");
    let mut config =
        SkillCatalogConfig::for_workspace(fixture.0.join("user"), Some(&fixture.0.join("project")));
    config.roots.extend([
        plugin_root(&fixture, "alpha", "alpha"),
        plugin_root(&fixture, "beta", "beta"),
    ]);
    config.roots.reverse();
    let catalog = SkillCatalog::discover(config);
    assert_eq!(catalog.skills().len(), 3);
    for (id, plugin_id, body) in [
        ("alpha:review", Some("alpha"), "ALPHA"),
        ("beta:review", Some("beta"), "BETA"),
        ("review", None, "PROJECT"),
    ] {
        let info = catalog
            .skills()
            .iter()
            .find(|skill| skill.id == id)
            .unwrap();
        assert_eq!(info.name, "review");
        assert_eq!(info.plugin_id.as_deref(), plugin_id);
        let loaded = catalog.load_skill(id).unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.name, "review");
        assert!(loaded.content.contains(body));
    }
    assert_eq!(catalog.diagnostics().len(), 1);
    assert_eq!(catalog.diagnostics()[0].code, "shadowed");
    assert!(catalog.prompt().contains("alpha:review"));
    assert!(!catalog.prompt().contains("ALPHA"));
}

#[test]
fn disabling_uses_exact_identifiers_without_cross_namespace_fallback() {
    let fixture = Fixture::new();
    fixture.skill("user/review", "review", "USER");
    fixture.skill("alpha/review", "review", "ALPHA");
    fixture.skill("beta/review", "review", "BETA");
    let mut config = SkillCatalogConfig::for_workspace(fixture.0.join("user"), None);
    config.roots.extend([
        plugin_root(&fixture, "alpha", "alpha"),
        plugin_root(&fixture, "beta", "beta"),
    ]);
    config.disabled_skill_ids.push("alpha:review".into());
    let catalog = SkillCatalog::discover(config.clone());
    assert!(catalog.load_skill("review").is_ok());
    assert!(catalog.load_skill("beta:review").is_ok());
    assert!(
        catalog
            .load_skill("alpha:review")
            .unwrap_err()
            .to_string()
            .contains("disabled")
    );
    assert!(
        catalog
            .read_skill_resource("alpha:review", "SKILL.md")
            .is_err()
    );
    assert!(!catalog.prompt().contains("alpha:review"));
    config.disabled_skill_ids = vec!["review".into()];
    let catalog = SkillCatalog::discover(config);
    assert!(catalog.load_skill("review").is_err());
    assert!(catalog.load_skill("alpha:review").is_ok());
    assert!(catalog.load_skill("beta:review").is_ok());
}

#[tokio::test]
async fn plugin_tools_load_instructions_and_only_the_selected_skills_resources() {
    let fixture = Fixture::new();
    for plugin in ["alpha", "beta"] {
        let skill = fixture.skill(&format!("{plugin}/review"), "review", plugin);
        fs::write(skill.join("guide.md"), plugin).unwrap();
    }
    let catalog = SkillCatalog::discover(SkillCatalogConfig {
        roots: vec![
            plugin_root(&fixture, "alpha", "alpha"),
            plugin_root(&fixture, "beta", "beta"),
        ],
        disabled_skill_ids: vec![],
    });
    let execute = |name: &str, arguments: &str| {
        catalog.execute(ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: arguments.into(),
        })
    };
    let list: serde_json::Value =
        serde_json::from_str(&execute(LIST_SKILLS_TOOL, "{}").await.unwrap()).unwrap();
    assert_eq!(list["skills"][0]["id"], "alpha:review");
    assert_eq!(list["skills"][0]["plugin_id"], "alpha");
    let loaded: serde_json::Value = serde_json::from_str(
        &execute(LOAD_SKILL_TOOL, r#"{"id":"beta:review"}"#)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(loaded["id"], "beta:review");
    assert_eq!(loaded["name"], "review");
    assert!(loaded["content"].as_str().unwrap().contains("beta"));
    let resource: serde_json::Value = serde_json::from_str(
        &execute(
            READ_SKILL_RESOURCE_TOOL,
            r#"{"id":"alpha:review","path":"guide.md"}"#,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(resource["skill_id"], "alpha:review");
    assert_eq!(resource["content"], "alpha");
    assert!(catalog.load_skill("review").is_err());
    assert!(
        catalog
            .read_skill_resource("alpha:review", "../../beta/review/guide.md")
            .is_err()
    );
    assert!(
        catalog
            .read_skill_resource("alpha:review", "/guide.md")
            .is_err()
    );
}

#[test]
fn roots_are_deduplicated_within_their_namespace_only() {
    let fixture = Fixture::new();
    fixture.skill("shared/review", "review", "SHARED");
    let mut duplicate = plugin_root(&fixture, "shared", "alpha");
    duplicate.scope = SkillScope::Project;
    let catalog = SkillCatalog::discover(SkillCatalogConfig {
        roots: vec![
            plugin_root(&fixture, "shared", "alpha"),
            plugin_root(&fixture, "shared", "beta"),
            duplicate,
        ],
        disabled_skill_ids: vec![],
    });
    assert_eq!(catalog.skills().len(), 2);
    assert_eq!(catalog.skills()[0].id, "alpha:review");
    assert_eq!(catalog.skills()[0].scope, SkillScope::Project);
    assert_eq!(catalog.skills()[1].id, "beta:review");
    assert!(catalog.diagnostics().is_empty());
}

#[test]
fn namespace_validation_preserves_full_length_qualified_ids() {
    let fixture = Fixture::new();
    let skill_name = "文".repeat(64);
    let plugin_id = "p".repeat(64);
    fixture.skill("valid/review", &skill_name, "VALID");
    fixture.skill("invalid/review", "review", "INVALID");
    let mut roots = vec![
        plugin_root(&fixture, "valid", &plugin_id),
        plugin_root(&fixture, "valid", "org.review"),
    ];
    let invalid = [
        "".to_owned(),
        "a:b".to_owned(),
        "../escape".to_owned(),
        " spaced".to_owned(),
        "中文".to_owned(),
        "p".repeat(65),
        ".".to_owned(),
        "..".to_owned(),
        "org..review".to_owned(),
    ];
    roots.extend(
        invalid
            .iter()
            .map(|id| plugin_root(&fixture, "invalid", id)),
    );
    let catalog = SkillCatalog::discover(SkillCatalogConfig {
        roots,
        disabled_skill_ids: vec![],
    });
    assert_eq!(catalog.skills().len(), 2);
    let id = format!("{plugin_id}:{skill_name}");
    assert!(catalog.skills().iter().any(|skill| skill.id == id));
    assert!(
        catalog
            .load_skill(&format!("org.review:{skill_name}"))
            .is_ok()
    );
    assert_eq!(id.chars().count(), 129);
    assert!(catalog.load_skill(&id).unwrap().content.contains("VALID"));
    assert_eq!(catalog.diagnostics().len(), invalid.len());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code == "invalid_namespace")
    );
}

#[test]
fn sixty_four_plugin_roots_fit_alongside_workspace_roots() {
    let fixture = Fixture::new();
    fixture.skill("user/personal", "personal", "USER");
    fixture.skill("project/.agents/skills/project", "project", "PROJECT");
    fixture.skill("project/.openharness/skills/other", "other", "OTHER");
    let mut config =
        SkillCatalogConfig::for_workspace(fixture.0.join("user"), Some(&fixture.0.join("project")));
    for index in 0..64 {
        let plugin = format!("plugin-{index}");
        fixture.skill(&format!("{plugin}/review"), "review", "PLUGIN");
        config.roots.push(plugin_root(&fixture, &plugin, &plugin));
    }
    let catalog = SkillCatalog::discover(config);
    assert_eq!(catalog.skills().len(), 67);
    assert!(catalog.diagnostics().is_empty());
}

#[test]
fn legacy_metadata_and_roots_default_to_no_plugin() {
    let root: SkillRoot = serde_json::from_value(serde_json::json!({
        "path": "/skills", "scope": "user",
    }))
    .unwrap();
    assert!(root.namespace.is_none());
    let info: SkillInfo = serde_json::from_value(serde_json::json!({
        "id": "review", "name": "review", "description": "Review",
        "path": "/skills/review/SKILL.md", "scope": "user", "enabled": true,
    }))
    .unwrap();
    assert!(info.plugin_id.is_none());
    assert!(
        serde_json::to_value(info)
            .unwrap()
            .get("plugin_id")
            .is_none()
    );
}
