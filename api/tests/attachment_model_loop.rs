//! Full provider -> runtime -> attachment-tool -> provider round trip. All
//! model responses and file contents are synthetic; no external API is called.
use agent_core::FileReference;
use api::{
    AppState,
    contracts::{ConfigureModel, CreateRunInput, RunDetail, RunStatus},
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn settled(state: &AppState, id: &str) -> RunDetail {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let detail = state.get(id).unwrap();
            if detail.summary.status != RunStatus::Running {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("attachment tool round trip did not finish")
}

fn tool_response(id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "id":format!("response-{id}"),
        "choices":[{"index":0,"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},
            "finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":500,"completion_tokens":30,"total_tokens":530}
    })
}

fn tool_result<'a>(request: &'a Value, id: &str) -> &'a str {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == id)
        .unwrap_or_else(|| panic!("missing tool result {id}: {request}"))["content"]
        .as_str()
        .unwrap()
}

#[tokio::test]
async fn large_original_is_read_on_demand_through_the_actual_model_loop_and_survives_restart() {
    let directory = Directory(std::env::temp_dir().join(format!(
        "openharness-attachment-model-loop-{}",
        std::process::id()
    )));
    let initial = AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    initial
        .set_plugin_enabled("builtin.llm-wiki".into(), false)
        .await
        .unwrap();

    let prefix = "UNREQUESTED_PADDING_LINE: regular delivery log.\n".repeat(16_000);
    let record = "FINAL_DELIVERY_REFERENCE: delivery completed 2026-10-28.\n";
    let content = format!("{prefix}{record}");
    assert!(content.len() > 512 * 1024);
    let upload = api::app(initial.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/attachments?name=delivery-log.txt")
                .header("content-type", "application/octet-stream")
                .body(Body::from(content.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let attachment: FileReference =
        serde_json::from_slice(&to_bytes(upload.into_body(), 8192).await.unwrap()).unwrap();
    let reference = attachment.attachment.as_ref().unwrap();
    assert_eq!(reference.size, content.len() as u64);
    assert!(attachment.text.is_empty());
    let path = format!("attachment:{}", reference.id);

    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let lookup_path = path.clone();
    let offset = prefix.chars().count();
    let mock = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let index = {
                let mut requests = captured.lock().unwrap();
                let index = requests.len();
                requests.push(body);
                index
            };
            let response = match index {
                0 => tool_response("search-delivery", "search_files", json!({"path":lookup_path,"query":"FINAL_DELIVERY_REFERENCE"})),
                1 => tool_response("read-delivery", "read_file", json!({"path":lookup_path,"cursor":{"unit":1,"offset":offset}})),
                2 => tool_response("original-delivery", "read_file", json!({"path":lookup_path,"mode":"original"})),
                _ => json!({"id":"final","choices":[{"index":0,"message":{"role":"assistant","content":"The saved delivery record says 2026-10-28."},"finish_reason":"stop"}],"usage":{"prompt_tokens":600,"completion_tokens":15,"total_tokens":615}}),
            };
            async move { Json(response) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let model = initial
        .configure_model(ConfigureModel {
            protocol: agent_providers::ProviderProtocol::ChatCompletions,
            base_url,
            api_key: "local-fixture-only".into(),
            model: "attachment-fixture".into(),
            context_window: 1_000_000,
            max_output_tokens: 4096,
            reasoning_mode: "off".into(),
            reasoning_effort: "medium".into(),
            reasoning_capability: None,
            reasoning_default: None,
            supports_images: false,
        })
        .await
        .unwrap();
    let created = initial
        .create(CreateRunInput {
            prompt: "Find the final delivery record in the attachment and quote its date.".into(),
            files: vec![attachment.clone()],
            model_id: model.id,
            workspace_path: None,
            permission_mode: Default::default(),
            reasoning_effort: None,
            execution_policy: Default::default(),
        })
        .await
        .unwrap();
    let id = created.summary.id;
    let completed = settled(&initial, &id).await;
    assert_eq!(
        completed.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        completed.error
    );
    assert!(completed.error.is_none());
    assert!(
        completed
            .messages
            .iter()
            .any(|message| message.role == "assistant"
                && message.text == "The saved delivery record says 2026-10-28.")
    );
    let user = completed
        .messages
        .iter()
        .find(|message| message.role == "user")
        .unwrap();
    let message_id = user.id.clone();
    assert_eq!(user.files[0].size, content.len());
    assert!(
        completed
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .all(|message| message.text.len() < 16 * 1024 && message.is_error != Some(true))
    );

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4, "unexpected request count");
        let first = requests[0].to_string();
        assert!(first.contains(&path));
        assert!(first.contains("delivery-log.txt"));
        assert!(!first.contains("UNREQUESTED_PADDING_LINE"));
        assert!(!first.contains(record.trim()));
        assert!(
            first.len() < 100 * 1024,
            "original file leaked into first model input"
        );
        let search: Value =
            serde_json::from_str(tool_result(&requests[1], "search-delivery")).unwrap();
        assert_eq!(search["path"], path);
        assert!(
            search["matches"][0]["text"]
                .as_str()
                .unwrap()
                .contains("2026-10-28")
        );
        assert_eq!(search["matches"][0]["line"], 16_001);
        let page: Value = serde_json::from_str(tool_result(&requests[2], "read-delivery")).unwrap();
        assert_eq!(page["content"], record);
        assert!(page["next_cursor"].is_null());
        let original: Value =
            serde_json::from_str(tool_result(&requests[3], "original-delivery")).unwrap();
        assert_eq!(original["read_only"], true);
        assert_eq!(original["name"], "delivery-log.txt");
        let local_path = original["local_path"].as_str().unwrap();
        assert_eq!(fs::read(local_path).unwrap(), content.as_bytes());
        assert!(
            requests
                .iter()
                .all(|request| !request.to_string().contains("UNREQUESTED_PADDING_LINE"))
        );
    }

    initial.shutdown().await;
    let reopened = AppState::open_with_delay(directory.0.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let response = api::app(reopened.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/runs/{id}/messages/{message_id}/files/0/content"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .as_ref(),
        content.as_bytes()
    );
    reopened.shutdown().await;
    server.abort();
}
