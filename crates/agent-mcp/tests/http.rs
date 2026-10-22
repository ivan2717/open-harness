use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_mcp::{McpClient, McpServerConfig, McpTransport};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct Counts {
    initializes: AtomicUsize,
    calls: AtomicUsize,
    deletes: AtomicUsize,
    headers: AtomicUsize,
}

struct Fixture {
    client: McpClient,
    counts: Arc<Counts>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture() -> Fixture {
    let counts = Arc::new(Counts::default());
    let app = Router::new()
        .route("/mcp", post(handle).delete(delete))
        .with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = McpClient::new(McpServerConfig {
        id: "http-fixture".into(),
        name: None,
        enabled: true,
        transport: McpTransport::Http {
            url,
            bearer_token_env: None,
        },
    })
    .unwrap()
    .with_timeout(Duration::from_secs(3));
    Fixture {
        client,
        counts,
        task,
    }
}

async fn delete(State(counts): State<Arc<Counts>>) -> StatusCode {
    counts.deletes.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn handle(
    State(counts): State<Arc<Counts>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request["method"].as_str().unwrap();
    if method == "notifications/initialized" || method == "notifications/cancelled" {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = request["id"].clone();
    let result = if method == "initialize" {
        counts.initializes.fetch_add(1, Ordering::SeqCst);
        json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}}, "serverInfo":{"name":"http-fixture","version":"1"}})
    } else {
        if headers.get("mcp-session-id").and_then(|h| h.to_str().ok()) != Some("fixture-session") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if headers
            .get("mcp-protocol-version")
            .and_then(|h| h.to_str().ok())
            == Some("2025-11-25")
        {
            counts.headers.fetch_add(1, Ordering::SeqCst);
        }
        match method {
            "tools/list" => json!({"tools":[{"name":"echo", "inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                counts.calls.fetch_add(1, Ordering::SeqCst);
                match request["params"]["name"].as_str().unwrap() {
                    "expired" => return StatusCode::NOT_FOUND.into_response(),
                    "hang" => tokio::time::sleep(Duration::from_secs(20)).await,
                    "oversize" => {
                        return (
                            [("content-type", "application/json")],
                            format!("{{\"huge\":\"{}\"}}", "x".repeat(5_000_000)),
                        )
                            .into_response();
                    }
                    "oversize_sse" => {
                        return (
                            [("content-type", "text/event-stream")],
                            format!("data: {}", "x".repeat(5_000_000)),
                        )
                            .into_response();
                    }
                    _ => {}
                }
                json!({"content":[{"type":"text","text":"hello"}], "structuredContent":{"ok":true}, "isError":false})
            }
            _ => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    let payload = json!({"jsonrpc":"2.0", "id":id, "result":result});
    if request["params"]["name"] == "sse" {
        (
            [("content-type", "text/event-stream")],
            format!("event: message\ndata: {payload}\n\n"),
        )
            .into_response()
    } else {
        ([("mcp-session-id", "fixture-session")], Json(payload)).into_response()
    }
}

#[tokio::test]
async fn json_and_sse_results_use_negotiated_headers_and_cleanup() {
    let fixture = fixture().await;
    assert_eq!(fixture.client.discover().await.unwrap().tools.len(), 1);
    for name in ["echo", "sse"] {
        let result = fixture.client.call_tool(name, Map::new()).await.unwrap();
        assert_eq!(result["content"][0]["text"], "hello");
        assert_eq!(result["structuredContent"]["ok"], true);
        assert_eq!(result["isError"], false);
    }
    assert!(fixture.counts.headers.load(Ordering::SeqCst) >= 3);
    assert_eq!(fixture.counts.initializes.load(Ordering::SeqCst), 1);
    fixture.client.shutdown().await;
    assert_eq!(fixture.counts.deletes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_sessions_do_not_replay_calls() {
    let fixture = fixture().await;
    assert!(
        fixture
            .client
            .call_tool("expired", Map::new())
            .await
            .is_err()
    );
    assert_eq!(fixture.counts.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bounds_json_sse_and_timeout() {
    let fixture = fixture().await;
    for name in ["oversize", "oversize_sse", "hang"] {
        assert!(
            fixture
                .client
                .clone()
                .with_timeout(Duration::from_millis(500))
                .call_tool(name, Map::new())
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.counts.calls.load(Ordering::SeqCst), 3);
}
