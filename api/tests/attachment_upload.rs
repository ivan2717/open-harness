//! Exercise the desktop upload boundary with synthetic data, never user files.
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

static SERIAL: AtomicU64 = AtomicU64::new(0);
const TOKEN: &str = "attachment-upload-test";
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn setup() -> (Directory, AppState, Router) {
    let directory = Directory(std::env::temp_dir().join(format!(
        "open-harness-attachment-http-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )));
    let state = AppState::open_with_delay(directory.0.clone(), Some(TOKEN.into()), Duration::ZERO)
        .await
        .unwrap();
    let app = api::app(state.clone());
    (directory, state, app)
}

async fn request(
    app: Router,
    method: &str,
    path: &str,
    body: Vec<u8>,
    auth: bool,
    origin: bool,
) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    if auth {
        request = request.header("authorization", format!("Bearer {TOKEN}"));
    }
    if origin {
        request = request.header("origin", "https://example.com");
    }
    request = request.header(
        "content-type",
        if path == "/api/runs" {
            "application/json"
        } else {
            "application/octet-stream"
        },
    );
    let response = app
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
}

#[tokio::test]
async fn upload_and_message_download_preserve_original_binary_without_inline_text() {
    let (_directory, state, app) = setup().await;
    // Deliberately not parseable PDF: import stores originals; reading is separate.
    let bytes = b"%PDF synthetic original\0binary\xff".repeat(20_000);
    let (status, body) = request(
        app.clone(),
        "POST",
        "/api/attachments?name=report.pdf",
        bytes.clone(),
        true,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let file: Value = serde_json::from_slice(&body).unwrap();
    assert!(file.get("text").is_none());
    assert!(file.get("path").is_none());
    assert_eq!(file["attachment"]["size"], bytes.len());
    let (status, body) = request(
        app.clone(),
        "POST",
        "/api/runs",
        serde_json::to_vec(&json!({"prompt":"Review", "model_id":"local-demo", "files":[file]}))
            .unwrap(),
        true,
        false,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let run: Value = serde_json::from_slice(&body).unwrap();
    let id = run["id"].as_str().unwrap();
    assert_eq!(run["messages"][0]["files"][0]["size"], bytes.len());
    let path = format!("/api/runs/{id}/messages/event-0/files/0/content");
    let (status, original) = request(app.clone(), "GET", &path, vec![], true, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(original, bytes);
    let invalid = format!("/api/runs/{id}/messages/event-0/files/1/content");
    assert_eq!(
        request(app, "GET", &invalid, vec![], true, false).await.0,
        StatusCode::NOT_FOUND
    );
    state.shutdown().await;
}

#[tokio::test]
async fn uploads_require_local_authenticated_client_and_valid_metadata() {
    let (_directory, state, app) = setup().await;
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/api/attachments?name=a.txt",
            b"x".to_vec(),
            false,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/api/attachments?name=a.txt",
            b"x".to_vec(),
            true,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/api/attachments?name=..%2Fsecret",
            b"x".to_vec(),
            true,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            app,
            "POST",
            "/api/attachments?name=a.txt&path=%2Fetc%2Fpasswd",
            vec![],
            true,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    state.shutdown().await;
}
