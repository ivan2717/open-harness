use agent_providers::ProviderProtocol;
use api::{
    AppState,
    contracts::{ConfigureModel, CreateRunInput, RunDetail, RunStatus},
};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const FACT: &str = "DECISION-2047: WAL was rejected after 3 concurrent writers; measured p99 = 17.375 ms, report = results/锁冲突-2047.csv. This only applies to the 3-writer fixture.";
const FOREIGN: &str = "OTHER-SESSION-PRIVATE-943";
const LOOKUP: &str =
    "Retrieve the original DECISION-2047 evidence and verify the history boundaries.";
static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "open-harness-wiki-disabled-history-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            directory,
            endpoint,
            requests,
            server,
        }
    }

    async fn open(&self) -> (AppState, String) {
        let state = AppState::open_with_delay(self.directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let model = state
            .configure_model(ConfigureModel {
                supports_images: false,
                protocol: ProviderProtocol::ChatCompletions,
                base_url: self.endpoint.clone(),
                api_key: "local-test-only".into(),
                model: "history-fixture".into(),
                context_window: 128_000,
                max_output_tokens: 4096,
                reasoning_mode: "off".into(),
                reasoning_effort: "high".into(),
                reasoning_capability: None,
                reasoning_default: None,
            })
            .await
            .unwrap();
        (state, model.id)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn tool(index: usize, id: &str, name: &str, arguments: Value) -> Value {
    json!({"role":"assistant", "content":null, "tool_calls":[{
        "id":format!("{id}:{index}"), "type":"function", "function":{"name":name,"arguments":arguments.to_string()}
    }]})
}

async fn respond(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let index = requests.lock().unwrap().len();
    let messages = request["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    let is_summary = messages.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("Summarize the supplied run events"))
    });
    let response = if is_summary {
        // Deliberately omit the exact historical detail: retrieval must recover
        // the journal evidence, not repeat a value preserved by the summary.
        json!({"role":"assistant", "content": json!({
            "objective":"Inspect the recorded experiment",
            "constraints":[], "completed_work":[], "current_state":"Experiment recorded",
            "decisions":[], "important_artifacts":[], "failed_attempts":[],
            "unresolved_questions":[], "current_plan":[], "next_action":"Verify historical evidence"
        }).to_string()})
    } else if last["role"] == "tool" {
        let output = last["content"].as_str().unwrap();
        match last["tool_call_id"]
            .as_str()
            .unwrap()
            .split(':')
            .next()
            .unwrap()
        {
            "find" => {
                let found: Value = serde_json::from_str(output).unwrap();
                let sequence = found["hits"][0]["sequence"].as_u64().unwrap();
                tool(
                    index,
                    "read",
                    "read_run_history",
                    json!({"from_sequence":sequence}),
                )
            }
            "read" => {
                let read: Value = serde_json::from_str(output).unwrap();
                tool(
                    index,
                    "past-boundary",
                    "read_run_history",
                    json!({
                        "from_sequence":read["through_sequence"].as_u64().unwrap() + 1
                    }),
                )
            }
            "past-boundary" => tool(
                index,
                "foreign-search",
                "search_run_history",
                json!({"query":FOREIGN}),
            ),
            "foreign-search" => tool(
                index,
                "foreign-read",
                "read_run_history",
                json!({
                    "from_sequence":0, "run_id":"another-run"
                }),
            ),
            "foreign-read" => {
                json!({"role":"assistant", "content":"Original evidence verified; no other session was accessible."})
            }
            unexpected => panic!("unexpected tool result {unexpected}"),
        }
    } else if last["content"] == LOOKUP {
        tool(
            index,
            "find",
            "search_run_history",
            json!({"query":"DECISION-2047"}),
        )
    } else {
        json!({"role":"assistant", "content":"Recorded. Awaiting the next request."})
    };
    let finish = if response["tool_calls"].is_array() {
        "tool_calls"
    } else {
        "stop"
    };
    // This fixture's reported usage must grow with the supplied instructions,
    // schemas and history. A constant 1200-token response makes the calibrated
    // pre-compaction input smaller than its fixed tool header; a rewritten
    // request correctly loses that exact-prefix measurement and cannot shrink
    // below it. The rough count here models usage, not an exact tokenizer.
    let prompt_tokens = json!({"messages":messages,"tools":request["tools"]})
        .to_string()
        .len()
        .div_ceil(4)
        + 16
        + messages.len() * 8
        + request["tools"]
            .as_array()
            .map_or(0, |tools| tools.len() * 16);
    let mut requests = requests.lock().unwrap();
    requests.push(request);
    Json(
        json!({"id":format!("response-{index}"), "model":"history-fixture",
            "choices":[{"index":0,"message":response,"finish_reason":finish}],
            "usage":{"prompt_tokens":prompt_tokens,"completion_tokens":100,"total_tokens":prompt_tokens + 100}
        }),
    )
}

async fn settled(state: &AppState, id: &str) -> RunDetail {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = state.get(id).unwrap();
            if detail.summary.status != RunStatus::Running && !detail.summary.compacting {
                assert!(detail.error.is_none(), "{:?}", detail.error);
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

async fn create(state: &AppState, model_id: &str, prompt: String) -> String {
    let run = state
        .create(CreateRunInput {
            reasoning_effort: None,
            execution_policy: Default::default(),
            prompt,
            files: Vec::new(),
            model_id: model_id.into(),
            workspace_path: None,
            permission_mode: Default::default(),
        })
        .await
        .unwrap();
    let id = run.summary.id;
    assert_eq!(
        settled(state, &id).await.summary.status,
        RunStatus::WaitingForInput
    );
    id
}

fn assert_retrieved(requests: &[Value]) {
    let result = |id: &str| {
        requests
            .iter()
            .find_map(|request| {
                let last = request["messages"].as_array()?.last()?;
                (last["role"] == "tool"
                    && last["tool_call_id"]
                        .as_str()
                        .is_some_and(|value| value.starts_with(&format!("{id}:"))))
                .then(|| last["content"].as_str().unwrap())
            })
            .unwrap_or_else(|| panic!("missing actual tool round {id}"))
    };
    let read: Value = serde_json::from_str(result("read")).unwrap();
    assert_eq!(read["events"][0]["sequence"], 0);
    assert!(read["events"][0]["event"].to_string().contains(FACT));
    assert!(!read.to_string().contains(FOREIGN));
    assert!(result("past-boundary").contains("past readable checkpoint sequence"));
    let foreign: Value = serde_json::from_str(result("foreign-search")).unwrap();
    assert_eq!(foreign["total_matches"], 0);
    assert!(result("foreign-read").contains("unknown field"));
    assert!(result("foreign-read").contains("run_id"));
    for request in requests {
        let names: Vec<_> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"search_run_history"));
        assert!(names.contains(&"read_run_history"));
        assert!(!names.contains(&"session_memory"));
    }
}

#[tokio::test]
async fn wiki_disabled_compaction_and_restart_keep_original_history_retrievable() {
    let fixture = Fixture::new().await;
    let (state, model_id) = fixture.open().await;
    state
        .set_plugin_enabled("builtin.llm-wiki".into(), false)
        .await
        .unwrap();
    let _other = create(&state, &model_id, format!("DECISION-2047: {FOREIGN}")).await;
    let id = create(
        &state,
        &model_id,
        format!("{FACT}\n{}", "Original experiment notes. ".repeat(130)),
    )
    .await;
    state
        .input(&id, "Prepare to inspect the earlier experiment.".into())
        .await
        .unwrap();
    settled(&state, &id).await;
    state.compact(&id).await.unwrap();
    let compacted = settled(&state, &id).await;
    assert!(compacted.context.as_ref().unwrap().compaction_count > 0);
    assert!(
        !compacted
            .context
            .as_ref()
            .unwrap()
            .checkpoints
            .iter()
            .any(|checkpoint| checkpoint.summary.contains(FACT))
    );

    let first = fixture.requests.lock().unwrap().len();
    state.input(&id, LOOKUP.into()).await.unwrap();
    assert_eq!(
        settled(&state, &id).await.summary.status,
        RunStatus::WaitingForInput
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert!(
            !requests[first]["messages"].to_string().contains(FACT),
            "the omitted value must come from history, not active context"
        );
        assert_retrieved(&requests[first..]);
    }
    state.shutdown().await;
    drop(state);

    let (restored, _) = fixture.open().await;
    assert!(!restored.extension_settings().config.llm_wiki_enabled);
    let after_restart = fixture.requests.lock().unwrap().len();
    restored.input(&id, LOOKUP.into()).await.unwrap();
    assert_eq!(
        settled(&restored, &id).await.summary.status,
        RunStatus::WaitingForInput
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert_retrieved(&requests[after_restart..]);
        let all = serde_json::to_string(&*requests).unwrap();
        assert!(
            !all.contains("OPENHARNESS_SESSION_WIKI_V1"),
            "Wiki must not compile while disabled"
        );
        assert!(!all.contains("Session memory: derived historical notes"));
    }
    assert_eq!(
        serde_json::to_value(restored.run_memory(&id).await.unwrap()).unwrap()["status"],
        "disabled"
    );
    restored.shutdown().await;
}
