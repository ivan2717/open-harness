use agent_core::agent::AgentRun;
use agent_runtime::{EventStore, JsonlEventStore};
use api::{
    AppState,
    contracts::{CreateRunInput, RunDetail, RunStatus},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tower::ServiceExt;

static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "open-harness-api-test-{}-{}",
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
async fn state(directory: &Directory) -> AppState {
    AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap()
}
async fn request(
    router: Router,
    method: &str,
    path: &str,
    value: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(value.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    let value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, value)
}
async fn settled(state: &AppState, id: &str) -> RunDetail {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = state.get(id).unwrap();
            if detail.summary.status != RunStatus::Running {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
async fn create(state: &AppState, prompt: &str) -> RunDetail {
    state
        .create(CreateRunInput {
            reasoning_effort: None,
            execution_policy: Default::default(),
            prompt: prompt.into(),
            files: Vec::new(),
            model_id: "local-demo".into(),
            workspace_path: None,
            permission_mode: Default::default(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn desktop_routes_use_real_runtime_compaction_and_stable_contracts() {
    let directory = Directory::new();
    let state = state(&directory).await;
    let app = api::app(state.clone());
    let (status, config) = request(app.clone(), "GET", "/api/config", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["default_model"], "local-demo");
    assert_eq!(config["models"][0]["context_window"], 8192);
    let (status, created) = request(
        app.clone(),
        "POST",
        "/api/runs",
        Some(json!({"prompt":"检查上下文管理", "model_id":"local-demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["status"], "running");
    assert!(created["revision"].is_u64());
    let id = created["id"].as_str().unwrap();
    let detail = settled(&state, id).await;
    assert_eq!(
        detail.summary.status,
        RunStatus::Completed,
        "{:?}",
        detail.error
    );
    let context = detail.context.as_ref().unwrap();
    assert!(context.compaction_count > 0);
    assert_eq!(context.compaction_count, context.checkpoints.len());
    assert!(
        context
            .checkpoints
            .iter()
            .all(|checkpoint| checkpoint.before_tokens > checkpoint.after_tokens)
    );
    assert!(context.estimated_input_tokens < context.input_budget);
    assert_eq!(context.measurement, "estimated");
    assert_eq!(
        detail
            .events
            .iter()
            .filter(|event| event.r#type == "tool_call_succeeded")
            .count(),
        4
    );
    assert!(
        detail
            .messages
            .iter()
            .any(|message| message.reasoning.is_some())
    );
    assert!(
        detail
            .messages
            .iter()
            .all(|message| message.status == "complete")
    );
    let (_, value) = request(app.clone(), "GET", &format!("/api/runs/{id}"), None).await;
    let keys = value.as_object().unwrap();
    for key in [
        "id",
        "title",
        "prompt",
        "status",
        "model_id",
        "created_at",
        "updated_at",
        "revision",
        "messages",
        "events",
        "context",
    ] {
        assert!(keys.contains_key(key), "missing {key}");
    }
    assert!(value["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(value["events"][0]["type"].is_string());
    assert!(!value.to_string().contains("provider_state"));
    assert!(!keys.contains_key("summary"));
    for _ in 0..2 {
        let (status, stopped) =
            request(app.clone(), "POST", &format!("/api/runs/{id}/cancel"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stopped["status"], "completed");
        assert_eq!(
            stopped["events"].as_array().unwrap().len(),
            detail.events.len()
        );
    }
    let (_, list) = request(app, "GET", "/api/runs", None).await;
    assert_eq!(list[0]["id"], id);
    assert!(list[0]["revision"].as_u64().unwrap() >= detail.summary.revision);
}

#[tokio::test]
async fn cancellation_discards_stream_and_prevents_further_tools() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(directory.0.clone(), None, Duration::from_millis(15))
        .await
        .unwrap();
    let created = create(&state, "取消运行").await;
    let id = &created.summary.id;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state
                .get(id)
                .unwrap()
                .messages
                .iter()
                .any(|message| message.status == "streaming")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = state.cancel(id).await.unwrap();
    assert_eq!(cancelled.summary.status, RunStatus::Cancelled);
    assert!(
        cancelled
            .messages
            .iter()
            .all(|message| message.status != "streaming")
    );
    let count = cancelled.events.len();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let detail = state.get(id).unwrap();
    assert_eq!(detail.events.len(), count);
    assert!(
        !detail
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_succeeded")
    );
    assert_eq!(detail.events.last().unwrap().r#type, "run_cancelled");
}

#[tokio::test]
async fn tool_stays_pending_until_its_result_is_committed() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(directory.0.clone(), None, Duration::from_millis(10))
        .await
        .unwrap();
    let created = create(&state, "观察工具执行").await;
    let id = &created.summary.id;
    let pending = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let detail = state.get(id).unwrap();
            if detail
                .messages
                .iter()
                .any(|message| message.role == "tool" && message.status == "pending")
            {
                break detail;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        pending
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_requested")
    );
    assert!(
        !pending
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_succeeded")
    );
    let cancelled = state.cancel(id).await.unwrap();
    let tool = cancelled
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .unwrap();
    assert_eq!(tool.status, "complete");
    assert_eq!(tool.is_error, Some(true));
    assert!(
        !cancelled
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_succeeded")
    );
}

#[tokio::test]
async fn waiting_input_restores_and_continues_after_restart() {
    let directory = Directory::new();
    let original = state(&directory).await;
    let run = create(&original, "/ask 先询问我").await;
    let id = &run.summary.id;
    let waiting = settled(&original, id).await;
    assert_eq!(waiting.summary.status, RunStatus::WaitingForInput);
    let restored = state(&directory).await;
    assert!(restored.get(id).unwrap().summary.revision > waiting.summary.revision);
    restored.input(id, "验证压缩".into()).await.unwrap();
    let finished = settled(&restored, id).await;
    assert_eq!(
        finished.summary.status,
        RunStatus::Completed,
        "{:?}",
        finished.error
    );
    let reopened = state(&directory).await;
    let saved = reopened.get(id).unwrap();
    assert_eq!(saved.summary.status, RunStatus::Completed);
    assert_eq!(saved.events.len(), finished.events.len());
    assert_eq!(
        saved.context.unwrap().compaction_count,
        finished.context.unwrap().compaction_count
    );
    assert!(reopened.input(id, "again".into()).await.is_err());
}

#[tokio::test]
async fn sse_sends_initial_and_reconnection_snapshots_with_auth() {
    let directory = Directory::new();
    let state = AppState::open_with_delay(
        directory.0.clone(),
        Some("integration-secret".into()),
        Duration::ZERO,
    )
    .await
    .unwrap();
    let created = create(&state, "SSE").await;
    let id = &created.summary.id;
    let app = api::app(state.clone());
    for path in [
        "/api/config".to_owned(),
        format!("/api/runs/{id}/events"),
        format!("/api/runs/{id}/messages/event-0/files/0"),
    ] {
        let (status, _) = request(app.clone(), "GET", &path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let snapshot = sse_snapshot(app.clone(), id).await;
    assert_eq!(snapshot["id"], id.as_str());
    assert!(snapshot["revision"].as_u64().unwrap() >= created.summary.revision);
    let completed = settled(&state, id).await;
    let reconnected = sse_snapshot(app.clone(), id).await;
    assert_eq!(reconnected["status"], "completed");
    assert!(reconnected["revision"].as_u64().unwrap() >= completed.summary.revision);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .header("authorization", "Bearer integration-secret")
                .header("origin", "https://untrusted.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

async fn sse_snapshot(app: Router, id: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{id}/events"))
                .header("authorization", "Bearer integration-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: snapshot"));
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    serde_json::from_str(data).unwrap()
}

#[tokio::test]
async fn interrupted_logs_restore_without_automatic_tool_execution() {
    let directory = Directory::new();
    let initial = state(&directory).await;
    let mut value = serde_json::to_value(create(&initial, "/ask fixture").await).unwrap();
    let original_id = value["id"].as_str().unwrap().to_owned();
    settled(&initial, &original_id).await;
    let id = "run-interrupted-fixture";
    value["id"] = json!(id);
    value["status"] = json!("running");
    value["revision"] = json!(800);
    fs::write(
        directory.0.join("runs").join(format!("{id}.json")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let run = AgentRun::new(id, "desktop-local-demo", "interrupted fixture");
    JsonlEventStore::new(directory.0.join("events"))
        .append(id, run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    let restarted = state(&directory).await;
    let restored = restarted.get(id).unwrap();
    assert_eq!(restored.summary.status, RunStatus::Interrupted);
    assert!(restored.summary.revision > 800);
    assert_eq!(restored.events.len(), 1);
    assert_eq!(
        restarted.cancel(id).await.unwrap().summary.status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn checkpoint_files_rebuild_stale_snapshot_compaction_records() {
    let directory = Directory::new();
    let original = state(&directory).await;
    let created = create(&original, "检查压缩恢复").await;
    let id = &created.summary.id;
    let completed = settled(&original, id).await;
    let expected = completed.context.as_ref().unwrap().checkpoints.clone();
    assert!(!expected.is_empty());
    let path = directory.0.join("runs").join(format!("{id}.json"));
    // Simulate a crash after the checkpoint file committed but before the UI
    // snapshot received its new compaction record.
    let mut stale = serde_json::to_value(&completed).unwrap();
    stale["context"]["checkpoints"] = json!([]);
    stale["context"]["compaction_count"] = json!(0);
    fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
    let restored = state(&directory).await.get(id).unwrap();
    let context = restored.context.as_ref().unwrap();
    assert_eq!(context.compaction_count, expected.len());
    for (actual, expected) in context.checkpoints.iter().zip(&expected) {
        assert_eq!(actual.id, expected.id);
        assert_eq!(actual.before_tokens, expected.before_tokens);
        assert_eq!(actual.after_tokens, expected.after_tokens);
        assert_eq!(actual.summary, expected.summary);
        assert!(actual.created_at.ends_with('Z'));
    }
    // Existing display timestamps are retained; counts, summary and token
    // measurements come from checkpoint storage, not a stale cached DTO.
    let mut stale = serde_json::to_value(restored).unwrap();
    stale["context"]["compaction_count"] = json!(500);
    stale["context"]["checkpoints"][0]["before_tokens"] = json!(0);
    stale["context"]["checkpoints"][0]["summary"] = json!("stale summary");
    stale["context"]["checkpoints"][0]["created_at"] = json!("2026-01-01T00:00:00.000Z");
    let mut phantom = stale["context"]["checkpoints"][0].clone();
    phantom["id"] = json!("nonexistent-checkpoint");
    stale["context"]["checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(phantom);
    fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
    let reconciled = state(&directory).await.get(id).unwrap().context.unwrap();
    assert_eq!(reconciled.compaction_count, expected.len());
    assert_eq!(
        reconciled.checkpoints[0].before_tokens,
        expected[0].before_tokens
    );
    assert_eq!(reconciled.checkpoints[0].summary, expected[0].summary);
    assert_eq!(
        reconciled.checkpoints[0].created_at,
        "2026-01-01T00:00:00.000Z"
    );
}

#[tokio::test]
async fn invalid_requests_are_rejected_without_runs() {
    let directory = Directory::new();
    let state = state(&directory).await;
    let app = api::app(state.clone());
    for body in [
        json!({"prompt":"", "model_id":"local-demo"}),
        json!({"prompt":"test", "model_id":"unknown"}),
    ] {
        let (status, _) = request(app.clone(), "POST", "/api/runs", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert!(state.list().is_empty());
    assert_eq!(
        request(app, "GET", "/api/runs/does-not-exist", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn file_references_survive_create_input_resume_and_restart_without_project_access() {
    use agent_core::agent::RunEvent;

    let directory = Directory::new();
    let initial = state(&directory).await;
    let app = api::app(initial.clone());
    let file = |name: &str, text: &str| json!({"name":name,"path":format!("/not-granted/{name}"),"text":text});
    let first = "创建时保存的内容 CREATE_SNAPSHOT";
    let second = "补充时保存的内容 INPUT_SNAPSHOT";
    let third = "继续时保存的内容 RESUME_SNAPSHOT";
    let (status, created) = request(
        app,
        "POST",
        "/api/runs",
        Some(json!({
            "prompt":"/ask 请查看引用文件",
            "model_id":"local-demo",
            "files":[file("first.rs",first)],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    assert_eq!(
        settled(&initial, id).await.summary.status,
        RunStatus::WaitingForInput
    );
    initial.shutdown().await;
    let reopened = state(&directory).await;
    let app = api::app(reopened.clone());
    let (status, input) = request(
        app.clone(),
        "POST",
        &format!("/api/runs/{id}/input"),
        Some(json!({"message":"", "files":[file("second.txt",second)]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!input.to_string().contains("INPUT_SNAPSHOT"));
    assert_eq!(
        settled(&reopened, id).await.summary.status,
        RunStatus::Completed
    );
    let (status, resumed) = request(
        app,
        "POST",
        &format!("/api/runs/{id}/resume"),
        Some(json!({"files":[file("third.md",third)]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!resumed.to_string().contains("RESUME_SNAPSHOT"));
    assert_eq!(
        settled(&reopened, id).await.summary.status,
        RunStatus::Completed
    );
    reopened.shutdown().await;
    let final_state = state(&directory).await;
    let detail = final_state.get(id).unwrap();
    assert_eq!(detail.summary.prompt, "/ask 请查看引用文件");
    assert!(detail.summary.workspace_path.is_none());
    assert!(final_state.workspace_review(id).await.is_err());
    let users: Vec<_> = detail
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .collect();
    assert_eq!(users.len(), 3);
    for (message, (name, text)) in users.iter().zip([
        ("first.rs", first),
        ("second.txt", second),
        ("third.md", third),
    ]) {
        assert_eq!(message.files.len(), 1);
        assert_eq!(message.files[0].name, name);
        assert_eq!(message.files[0].size, text.len());
        assert_eq!(
            message.files[0].path.as_deref(),
            Some(format!("/not-granted/{name}").as_str())
        );
        assert!(!message.text.contains("SNAPSHOT"));
        let (status, snapshot) = request(
            api::app(final_state.clone()),
            "GET",
            &format!("/api/runs/{id}/messages/{}/files/0", message.id),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(snapshot["name"], name);
        assert_eq!(
            snapshot["text"], text,
            "preview reads the saved event, not the path"
        );
    }
    for path in [
        format!("/api/runs/{id}/messages/{}/files/1", users[0].id),
        format!("/api/runs/{id}/messages/event-999999/files/0"),
        format!("/api/runs/{id}/messages/event-1/files/0"),
        "/api/runs/does-not-exist/messages/event-0/files/0".into(),
    ] {
        assert_eq!(
            request(api::app(final_state.clone()), "GET", &path, None)
                .await
                .0,
            StatusCode::NOT_FOUND,
        );
    }
    assert_eq!(users[1].text, "");
    assert_eq!(users[2].text, "");
    let display = serde_json::to_string(&detail).unwrap();
    assert!(
        !display.contains("SNAPSHOT"),
        "snapshots are not expanded in UI DTOs"
    );
    let stored = JsonlEventStore::new(directory.0.join("events"))
        .load(id)
        .await
        .unwrap()
        .unwrap();
    let snapshots: Vec<_> = stored
        .events
        .iter()
        .filter_map(|event| match &event.event {
            RunEvent::RunStarted { files, .. }
            | RunEvent::InputReceived { files, .. }
            | RunEvent::RunResumed { files, .. } => Some(files[0].text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots, vec![first, second, third]);
    final_state.shutdown().await;
}

#[tokio::test]
async fn file_only_messages_use_filenames_for_titles_and_invalid_files_do_not_create_runs() {
    let directory = Directory::new();
    let state = state(&directory).await;
    let app = api::app(state.clone());
    let file = json!({"name":"notes.md","text":"FILE_BODY_SENTINEL"});
    for files in [
        json!([{"name":"", "text":"x"}]),
        json!([{"name":"../notes.md", "text":"x"}]),
        json!([{"name":"notes.md", "path":"bad\npath", "text":"x"}]),
        json!([{"name":"notes.md", "text":"binary\u{0000}value"}]),
        json!([{"name":"notes.md", "text":"x".repeat(256 * 1024 + 1)}]),
        json!((0..9).map(|_| file.clone()).collect::<Vec<_>>()),
        json!(
            (0..3)
                .map(|_| json!({"name":"notes.md", "text":"x".repeat(200 * 1024)}))
                .collect::<Vec<_>>()
        ),
    ] {
        let (status, _) = request(
            app.clone(),
            "POST",
            "/api/runs",
            Some(json!({
                "prompt":"read files", "model_id":"local-demo", "files":files,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert!(state.list().is_empty());
    let (status, created) = request(
        app,
        "POST",
        "/api/runs",
        Some(json!({
            "prompt":"", "model_id":"local-demo", "files":[file],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["title"], "notes.md");
    assert_eq!(created["prompt"], "");
    assert_eq!(created["messages"][0]["files"][0]["name"], "notes.md");
    assert!(!created.to_string().contains("FILE_BODY_SENTINEL"));
    state.shutdown().await;
}
