use api::AppState;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tower::ServiceExt;

static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "open-harness-skills-route-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
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
fn skill(root: &Path, name: &str) {
    let directory = root.join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Review documents\n---\nPRIVATE_SKILL_BODY\n"),
    )
    .unwrap();
}
fn catalog_url(path: &Path) -> String {
    let encoded: String = path
        .to_string_lossy()
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    format!("/api/extensions/skills?workspace_path={encoded}")
}
async fn get(router: &Router, path: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    let body =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, body)
}

#[tokio::test]
async fn fresh_catalog_includes_project_and_plugin_metadata_without_creating_a_run_or_mutating_config()
 {
    let directory = Directory::new();
    let data = directory.0.join("state");
    let project = directory.0.join("项目 & notes");
    skill(&data.join("skills"), "personal");
    skill(&project.join(".agents/skills"), "review");
    let source = directory.0.join("plugin");
    skill(&source.join("skills"), "review");
    fs::create_dir_all(source.join(".codex-plugin")).unwrap();
    fs::write(
        source.join(".codex-plugin/plugin.json"),
        json!({"name":"sample", "version":"1.0.0", "skills":"./skills"}).to_string(),
    )
    .unwrap();
    let state = AppState::open_with_delay(data.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    state
        .install_plugin(source.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    let mut config = state.extension_settings().config;
    config.disabled_skill_ids = vec!["review".into()];
    state.save_extensions(config).unwrap();
    let saved = fs::read(data.join("extensions.json")).unwrap();
    let app = api::app(state.clone());

    let (status, global) = get(&app, "/api/extensions/skills", &[]).await;
    assert_eq!(status, StatusCode::OK, "{global}");
    let ids: Vec<_> = global["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"personal"));
    assert!(ids.contains(&"sample:review"));

    let (status, project_catalog) = get(&app, &catalog_url(&project), &[]).await;
    assert_eq!(status, StatusCode::OK, "{project_catalog}");
    let skills = project_catalog["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 3);
    let review = skills.iter().find(|s| s["id"] == "review").unwrap();
    assert_eq!(review["enabled"], false);
    assert_eq!(review["scope"], "project");
    let plugin = skills.iter().find(|s| s["id"] == "sample:review").unwrap();
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["plugin_id"], "sample");
    assert!(!project_catalog.to_string().contains("PRIVATE_SKILL_BODY"));
    assert!(project_catalog.get("approvals").is_none());
    assert!(project_catalog.get("config").is_none());

    skill(&project.join(".agents/skills"), "later");
    let (status, refreshed) = get(&app, &catalog_url(&project), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        refreshed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "later")
    );
    assert!(state.list().is_empty());
    assert_eq!(fs::read(data.join("extensions.json")).unwrap(), saved);
}

#[tokio::test]
async fn catalog_rejects_invalid_and_private_workspace_roots() {
    let directory = Directory::new();
    let data = directory.0.join("state");
    let state = AppState::open_with_delay(data.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let file = directory.0.join("not-a-folder");
    fs::write(&file, "text").unwrap();
    let app = api::app(state);
    for path in [
        PathBuf::from("relative"),
        directory.0.join("missing"),
        file,
        data.clone(),
        data.join("skills"),
        directory.0.clone(),
    ] {
        let (status, body) = get(&app, &catalog_url(&path), &[]).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{}: {body}",
            path.display()
        );
    }
    let (status, _) = get(&app, "/api/extensions/skills?workspace_path=%00", &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalog_is_authenticated_and_rejects_browser_origins() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(
        directory.0.clone(),
        Some("skills-token".into()),
        Duration::ZERO,
    )
    .await
    .unwrap();
    let app = api::app(state);
    let (status, _) = get(&app, "/api/extensions/skills", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let auth = [("authorization", "Bearer skills-token")];
    let (status, _) = get(&app, "/api/extensions/skills", &auth).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(
        &app,
        "/api/extensions/skills",
        &[
            ("authorization", "Bearer skills-token"),
            ("origin", "https://example.com"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
