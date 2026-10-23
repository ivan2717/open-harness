use api::AppState;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tower::ServiceExt;

const TOKEN: &str = "plugin-route-test-token";
const AUTH: &[(&str, &str)] = &[("authorization", "Bearer plugin-route-test-token")];
static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "open-harness-plugin-routes-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    async fn state(&self) -> AppState {
        AppState::open_with_delay(self.0.join("state"), Some(TOKEN.into()), Duration::ZERO)
            .await
            .unwrap()
    }

    fn package(&self, version: &str) -> PathBuf {
        let source = self.0.join("source");
        fs::create_dir_all(source.join(".codex-plugin")).unwrap();
        fs::create_dir_all(source.join("skills/review")).unwrap();
        fs::write(
            source.join(".codex-plugin/plugin.json"),
            json!({
                "name": "org.review", "version": version,
                "description": "Document review", "skills": "./skills",
                "mcpServers": "./.mcp.json"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            source.join(".mcp.json"),
            json!({"mcpServers": {"documents": {
                "command": "nonexistent-plugin-route-fixture-must-not-start", "args": []
            }}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            source.join("skills/review/SKILL.md"),
            format!(
                "---\nname: review\ndescription: Review documents\n---\nPRIVATE BODY {version}\n"
            ),
        )
        .unwrap();
        source
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    value: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(
            builder
                .body(value.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    let body =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, body)
}

async fn settings(router: &Router) -> Value {
    let (status, settings) = request(router, "GET", "/api/extensions", None, AUTH).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    settings
}

#[tokio::test]
async fn builtin_memory_is_default_on_revocable_persistent_and_inspection_is_authenticated() {
    let directory = Directory::new();
    let state = directory.state().await;
    let app = api::app(state.clone());
    let initial = settings(&app).await;
    assert_eq!(initial["config"]["llm_wiki_enabled"], true);
    assert_eq!(initial["builtin_plugins"][0]["id"], "builtin.llm-wiki");
    assert_eq!(initial["builtin_plugins"][0]["enabled"], true);
    let (status, run) = request(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"prompt":"memory inspection", "model_id":"local-demo"})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let path = format!("/api/runs/{}/memory", run["id"].as_str().unwrap());
    assert_eq!(
        request(&app, "GET", &path, None, &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    let mut headers = AUTH.to_vec();
    headers.push(("origin", "https://example.test"));
    assert_eq!(
        request(&app, "GET", &path, None, &headers).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, empty) = request(&app, "GET", &path, None, AUTH).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["status"], "empty");
    assert_eq!(empty["revision"], 0);
    assert_eq!(
        request(&app, "GET", "/api/runs/missing/memory", None, AUTH)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, disabled) = request(
        &app,
        "POST",
        "/api/plugins/builtin.llm-wiki/enabled",
        Some(json!({"enabled":false})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["config"]["llm_wiki_enabled"], false);
    assert_eq!(disabled["builtin_plugins"][0]["enabled"], false);
    assert_eq!(
        request(&app, "GET", &path, None, AUTH).await.1["status"],
        "disabled"
    );
    assert_eq!(
        request(&app, "DELETE", "/api/plugins/builtin.llm-wiki", None, AUTH)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    state.shutdown().await;
    let restored = directory.state().await;
    assert!(!restored.extension_settings().config.llm_wiki_enabled);
    restored.shutdown().await;
}

#[tokio::test]
async fn plugin_mutation_routes_require_auth_and_reject_browser_origins() {
    let directory = Directory::new();
    let source = directory.package("1.0.0");
    let state = directory.state().await;
    let app = api::app(state.clone());
    for (method, path, body) in [
        (
            "POST",
            "/api/plugins/install",
            Some(json!({"source_path": source})),
        ),
        (
            "POST",
            "/api/plugins/org.review/enabled",
            Some(json!({"enabled": false})),
        ),
        ("DELETE", "/api/plugins/org.review", None),
    ] {
        for headers in [vec![], vec![("authorization", "Bearer wrong-token")]] {
            let (status, _) = request(&app, method, path, body.clone(), &headers).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
        let mut headers = AUTH.to_vec();
        headers.push(("origin", "https://untrusted.example"));
        let (status, _) = request(&app, method, path, body, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert!(
        settings(&app).await["plugins"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    state.shutdown().await;
}

#[tokio::test]
async fn plugin_http_lifecycle_preserves_manual_extensions_and_persists_updates() {
    let directory = Directory::new();
    let source = directory.package("1.0.0");
    let state = directory.state().await;
    let personal = directory.0.join("state/skills/review");
    fs::create_dir_all(&personal).unwrap();
    fs::write(
        personal.join("SKILL.md"),
        "---\nname: review\ndescription: Personal review\n---\nPersonal instructions\n",
    )
    .unwrap();
    let app = api::app(state.clone());
    let mut config = settings(&app).await["config"].clone();
    config["mcp_servers"] = json!([{
        "id": "manual", "name": "Manual server", "enabled": true,
        "transport": {"type": "stdio", "command": "nonexistent-manual-route-fixture"}
    }]);
    let (status, saved) = request(&app, "POST", "/api/extensions", Some(config), AUTH).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let manual_config = saved["config"].clone();
    let (status, installed) = request(
        &app,
        "POST",
        "/api/plugins/install",
        Some(json!({"source_path": source})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(installed["plugins"].as_array().unwrap().len(), 1);
    assert_eq!(installed["plugins"][0]["id"], "org.review");
    assert_eq!(installed["plugins"][0]["version"], "1.0.0");
    assert_eq!(installed["plugins"][0]["enabled"], true);
    assert_eq!(
        installed["plugins"][0]["mcp_servers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(installed["config"], manual_config);
    assert_eq!(installed["skills"].as_array().unwrap().len(), 2);
    assert!(
        installed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["id"] == "org.review:review" && skill["plugin_id"] == "org.review")
    );
    assert!(!installed.to_string().contains("PRIVATE BODY"));
    let old_hash = installed["plugins"][0]["content_hash"].clone();
    let old_server_id = installed["plugins"][0]["mcp_servers"][0]["id"].clone();

    let (status, disabled) = request(
        &app,
        "POST",
        "/api/plugins/org.review/enabled",
        Some(json!({"enabled": false})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["plugins"][0]["enabled"], false);
    assert_eq!(disabled["skills"].as_array().unwrap().len(), 1);
    assert_eq!(disabled["skills"][0]["id"], "review");
    state.shutdown().await;

    let reopened = directory.state().await;
    let app = api::app(reopened.clone());
    assert_eq!(settings(&app).await["plugins"][0]["enabled"], false);
    directory.package("2.0.0");
    assert_eq!(settings(&app).await["plugins"][0]["version"], "1.0.0");
    let (status, updated) = request(
        &app,
        "POST",
        "/api/plugins/install",
        Some(json!({"source_path": source, "expected_id": "org.review"})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["plugins"][0]["version"], "2.0.0");
    assert_eq!(updated["plugins"][0]["enabled"], false);
    assert_ne!(updated["plugins"][0]["content_hash"], old_hash);
    assert_eq!(updated["plugins"][0]["mcp_servers"][0]["id"], old_server_id);
    assert_eq!(updated["config"], manual_config);
    let (status, enabled) = request(
        &app,
        "POST",
        "/api/plugins/org.review/enabled",
        Some(json!({"enabled": true})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["skills"].as_array().unwrap().len(), 2);
    let (status, removed) = request(&app, "DELETE", "/api/plugins/org.review", None, AUTH).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(removed["plugins"].as_array().unwrap().is_empty());
    assert_eq!(removed["skills"][0]["id"], "review");
    assert_eq!(removed["config"], manual_config);
    reopened.shutdown().await;

    let final_state = directory.state().await;
    let final_settings = settings(&api::app(final_state.clone())).await;
    assert!(final_settings["plugins"].as_array().unwrap().is_empty());
    assert_eq!(final_settings["config"], manual_config);
    final_state.shutdown().await;
}

#[tokio::test]
async fn invalid_plugin_mutations_do_not_replace_the_installed_version() {
    let directory = Directory::new();
    let source = directory.package("1.0.0");
    let state = directory.state().await;
    let app = api::app(state.clone());
    let (status, installed) = request(
        &app,
        "POST",
        "/api/plugins/install",
        Some(json!({"source_path": source})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    directory.package("2.0.0");
    for (method, path, body) in [
        (
            "POST",
            "/api/plugins/install",
            Some(json!({"source_path": "relative/path"})),
        ),
        (
            "POST",
            "/api/plugins/install",
            Some(json!({"source_path": source})),
        ),
        (
            "POST",
            "/api/plugins/install",
            Some(json!({"source_path": source, "expected_id": "another-plugin"})),
        ),
        (
            "POST",
            "/api/plugins/missing/enabled",
            Some(json!({"enabled": false})),
        ),
        ("DELETE", "/api/plugins/missing", None),
    ] {
        let (status, result) = request(&app, method, path, body, AUTH).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path}: {result}");
        assert_eq!(settings(&app).await["plugins"], installed["plugins"]);
    }
    for (path, body) in [
        (
            "/api/plugins/install",
            json!({"source_path": source, "execute": true}),
        ),
        ("/api/plugins/org.review/enabled", json!({"enabled": "yes"})),
        (
            "/api/plugins/org.review/enabled",
            json!({"enabled": false, "run": "command"}),
        ),
    ] {
        let (status, _) = request(&app, "POST", path, Some(body), AUTH).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    fs::write(source.join(".codex-plugin/plugin.json"), "invalid json").unwrap();
    let (status, _) = request(
        &app,
        "POST",
        "/api/plugins/install",
        Some(json!({"source_path": source, "expected_id": "org.review"})),
        AUTH,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(settings(&app).await["plugins"], installed["plugins"]);
    state.shutdown().await;
}

#[tokio::test]
async fn shutdown_rejects_new_plugin_mutations() {
    let directory = Directory::new();
    let source = directory.package("1.0.0");
    let state = directory.state().await;
    let app = api::app(state.clone());
    state.shutdown().await;
    for (method, path, body) in [
        (
            "POST",
            "/api/plugins/install",
            Some(json!({"source_path": source})),
        ),
        (
            "POST",
            "/api/plugins/org.review/enabled",
            Some(json!({"enabled": true})),
        ),
        ("DELETE", "/api/plugins/org.review", None),
    ] {
        let (status, result) = request(&app, method, path, body, AUTH).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path}: {result}");
    }
    assert!(
        settings(&app).await["plugins"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
