//! End-to-end coordination through the actual HTTP adapter and persisted service.
//! The scripted provider controls timing, not execution or permission decisions.
use agent_local::PermissionMode;
use agent_providers::ProviderProtocol;
use api::{
    AppState,
    contracts::{ConfigureModel, CreateRunInput, ExecutionPolicy, RunDetail, RunStatus},
};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Reply {
    message: Value,
    gate: Option<Arc<Semaphore>>,
}

impl Reply {
    fn done(text: &str) -> Self {
        Self {
            message: json!({"role":"assistant", "content":text}),
            gate: None,
        }
    }
    fn tools(calls: Vec<Value>) -> Self {
        Self {
            message: json!({"role":"assistant", "content":null, "tool_calls":calls}),
            gate: None,
        }
    }
    fn blocked(mut self, gate: &Arc<Semaphore>) -> Self {
        self.gate = Some(gate.clone());
        self
    }
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id, "type":"function", "function":{"name":name, "arguments":args.to_string()}})
}

fn spawn(id: &str, task: &str, paths: &[&str]) -> Value {
    call(
        id,
        "spawn_agent",
        json!({"task_name":task,"task":task,"write_paths":paths}),
    )
}

type Script = dyn Fn(&str, usize, &Value) -> Reply + Send + Sync;
struct Provider {
    requests: Mutex<Vec<Value>>,
    counts: Mutex<BTreeMap<String, usize>>,
    script: Box<Script>,
    mcp_calls: AtomicU64,
}

fn actor(request: &Value) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .expect("first user message is the run's prompt")
        .to_owned()
}

async fn respond(State(provider): State<Arc<Provider>>, Json(request): Json<Value>) -> Json<Value> {
    let actor = actor(&request);
    let index = {
        let mut counts = provider.counts.lock().unwrap();
        let count = counts.entry(actor.clone()).or_default();
        let index = *count;
        *count += 1;
        index
    };
    provider.requests.lock().unwrap().push(request.clone());
    let reply = (provider.script)(&actor, index, &request);
    if let Some(gate) = reply.gate {
        gate.acquire().await.unwrap().forget();
    }
    let finish = if reply.message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    Json(
        json!({"id":format!("response-{index}"), "model":"coordination-fixture",
        "choices":[{"index":0,"message":reply.message,"finish_reason":finish}],
        "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}}),
    )
}

async fn mcp_respond(
    State(provider): State<Arc<Provider>>,
    Json(request): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if request.get("id").is_none() {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"delegation-fixture","version":"1"}})
        }
        "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
        "tools/call" => {
            provider.mcp_calls.fetch_add(1, Ordering::SeqCst);
            json!({"content":[{"type":"text","text":"APPROVED_CHILD_MCP_RESULT"}],"isError":false})
        }
        method => panic!("unexpected MCP method {method}"),
    };
    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
}

struct Fixture {
    directory: PathBuf,
    project: PathBuf,
    endpoint: String,
    provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(script: impl Fn(&str, usize, &Value) -> Reply + Send + Sync + 'static) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-collaboration-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        for index in 0..20 {
            fs::write(
                project.join(format!("file-{index}.txt")),
                format!("Evidence {index}\n"),
            )
            .unwrap();
        }
        let provider = Arc::new(Provider {
            requests: Mutex::new(Vec::new()),
            counts: Mutex::new(BTreeMap::new()),
            script: Box::new(script),
            mcp_calls: AtomicU64::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .route("/mcp", post(mcp_respond))
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            directory,
            project,
            endpoint,
            provider,
            server,
        }
    }

    async fn state(&self) -> AppState {
        let state = AppState::open_with_delay(self.directory.join("state"), None, Duration::ZERO)
            .await
            .unwrap();
        state
            .set_plugin_enabled("builtin.llm-wiki".into(), false)
            .await
            .unwrap();
        state
    }

    async fn configure(&self, state: &AppState) -> String {
        state
            .configure_model(ConfigureModel {
                protocol: ProviderProtocol::ChatCompletions,
                base_url: self.endpoint.clone(),
                api_key: "local-test-only".into(),
                model: "coordination-fixture".into(),
                context_window: 128_000,
                max_output_tokens: 4096,
                reasoning_mode: "off".into(),
                reasoning_effort: "high".into(),
                reasoning_capability: None,
                reasoning_default: None,
                supports_images: false,
            })
            .await
            .unwrap()
            .id
    }

    fn input(&self, model_id: String, prompt: &str) -> CreateRunInput {
        CreateRunInput {
            reasoning_effort: None,
            prompt: prompt.into(),
            model_id,
            files: Vec::new(),
            workspace_path: Some(self.project.display().to_string()),
            permission_mode: PermissionMode::ReadOnly,
            execution_policy: ExecutionPolicy::default(),
        }
    }

    fn requests_for(&self, name: &str) -> Vec<Value> {
        self.provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| actor(r) == name)
            .cloned()
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("coordination did not reach the expected state");
}

async fn settled(state: &AppState, id: &str) -> RunDetail {
    until(|| state.get(id).unwrap().summary.status != RunStatus::Running).await;
    state.get(id).unwrap()
}

// Terminal/waiting status is published before the execution wrapper finishes
// delivery and lease cleanup. Subsequent work must wait for admission to reopen,
// while unexpected errors still fail immediately rather than being retried.
async fn after_background_settles<F, Fut, T, E>(mut operation: F, pending_message: &str) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display + axum::response::IntoResponse,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match operation().await {
                Ok(result) => return result,
                Err(error) => {
                    assert_eq!(error.to_string(), pending_message);
                    assert_eq!(
                        error.into_response().status(),
                        axum::http::StatusCode::CONFLICT
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    })
    .await
    .expect("execution wrapper did not finish its background cleanup")
}

fn spawned_id(request: &Value, call_id: &str) -> String {
    let result = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
        .expect("spawn result is present");
    serde_json::from_str::<Value>(result["content"].as_str().unwrap()).unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn has_result(request: &Value, call_id: &str) -> bool {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
}

#[tokio::test]
async fn child_can_recover_truncated_parent_messages_without_private_state() {
    fn tool_result(request: &Value, id: &str) -> Value {
        let message = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool" && message["tool_call_id"] == id)
            .unwrap();
        serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
    }
    let fixture = Fixture::new(|who, index, request| {
        if who != "CHILD" {
            return if index == 0 {
                Reply::tools(vec![spawn("child", "CHILD", &[])])
            } else { Reply::done("Parent integrates findings") };
        }
        match index {
            0 => Reply::tools(vec![call("members", "agent_control", json!({"action":"list"}))]),
            1 => {
                let members = tool_result(request, "members");
                let root = members["members"].as_array().unwrap().iter().find(|member| member["delegation"].is_null()).unwrap();
                Reply::tools(vec![call("read", "agent_control", json!({"action":"read","agent_id":root["agent_id"],"limit":1}))])
            },
            2 => {
                let read = tool_result(request, "read");
                assert_eq!(read["messages"][0]["truncated"], true);
                Reply::tools(vec![call("tail", "agent_control", json!({"action":"read","agent_id":read["agent_id"],"message_id":read["messages"][0]["id"],"text_offset":3900,"text_limit":100}))])
            },
            _ => {
                let tail = tool_result(request, "tail");
                assert_eq!(tail["text"], "LATEST_RULE");
                assert!(tail["next_text_offset"].is_null());
                assert!(tail.get("reasoning").is_none());
                Reply::done("Recovered latest rule")
            }
        }
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, &format!("{}LATEST_RULE", "甲".repeat(3900))))
        .await
        .unwrap()
        .summary
        .id;
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert!(
        result
            .events
            .iter()
            .any(|event| event.detail.contains("Recovered latest rule"))
    );
    state.shutdown().await;
}

#[tokio::test]
async fn child_inherits_workspace_and_mcp_while_parent_handles_its_approval() {
    let fixture = Fixture::new(|who, index, request| match (who, index) {
        ("ROOT", 0) => Reply::tools(vec![call("child", "spawn_agent", json!({"task_name":"Research", "task":"CHILD"}))]),
        ("CHILD", 0) => {
            let tools = request["tools"].as_array().unwrap();
            assert!(tools.iter().any(|tool| tool["function"]["name"] == "call_mcp_tool"));
            assert!(tools.iter().any(|tool| tool["function"]["name"] == "apply_patch"));
            Reply::tools(vec![call("discover", "list_mcp_tools", json!({"server_id":"fixture"}))])
        },
        ("CHILD", 1) => Reply::tools(vec![
            call("write", "apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: child-result.txt\n+Inherited workspace permission\n*** End Patch"})),
            call("external", "call_mcp_tool", json!({"server_id":"fixture","name":"echo","arguments":{}})),
        ]),
        ("CHILD", _) => {
            assert!(request.to_string().contains("APPROVED_CHILD_MCP_RESULT"));
            Reply::done("MCP evidence verified")
        },
        _ => Reply::done("Parent integrates evidence"),
    }).await;
    let state = fixture.state().await;
    let mut config = state.extension_settings().config;
    config.mcp_servers.push(agent_mcp::McpServerConfig {
        id: "fixture".into(),
        name: Some("Fixture service".into()),
        enabled: true,
        transport: agent_mcp::McpTransport::Http {
            url: fixture.endpoint.replace("/v1", "/mcp"),
            bearer_token_env: None,
        },
    });
    state.save_extensions(config).unwrap();
    let model = fixture.configure(&state).await;
    let mut unrelated_input = fixture.input(model.clone(), "UNRELATED");
    unrelated_input.workspace_path = None;
    let unrelated = state.create(unrelated_input).await.unwrap().summary.id;
    let mut input = fixture.input(model, "ROOT");
    input.permission_mode = PermissionMode::WorkspaceWrite;
    let root = state.create(input).await.unwrap().summary.id;
    until(|| !state.run_extensions(&root).unwrap().approvals.is_empty()).await;
    let approval = state.run_extensions(&root).unwrap().approvals.remove(0);
    let child = approval
        .source_run_id
        .as_ref()
        .expect("parent can identify approval owner");
    assert_eq!(approval.task_title.as_deref(), Some("Research"));
    assert_eq!(
        state.get(child).unwrap().summary.permission_mode,
        PermissionMode::WorkspaceWrite
    );
    assert!(
        state
            .get(child)
            .unwrap()
            .summary
            .delegation
            .unwrap()
            .write_paths
            .is_none()
    );
    assert!(fixture.project.join("child-result.txt").is_file());
    assert_eq!(fixture.provider.mcp_calls.load(Ordering::SeqCst), 0);
    assert!(
        state
            .approve_extension(&unrelated, &approval.id, true)
            .is_err()
    );
    state.approve_extension(&root, &approval.id, true).unwrap();
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert_eq!(fixture.provider.mcp_calls.load(Ordering::SeqCst), 1);
    assert!(state.run_extensions(&root).unwrap().approvals.is_empty());
    assert!(
        result
            .events
            .iter()
            .any(|event| event.detail.contains("MCP evidence verified"))
    );
    state.shutdown().await;
}

#[tokio::test]
async fn children_run_concurrently_with_public_handoff_and_parent_waits_for_durable_results() {
    let gate = Arc::new(Semaphore::new(0));
    let child_gate = gate.clone();
    let fixture = Fixture::new(move |who, index, _| match who {
        "ROOT_PRIVATE_SENTINEL" if index == 0 => Reply::tools(vec![
            spawn("alpha", "CHILD_ALPHA", &[]),
            spawn("beta", "CHILD_BETA", &[]),
        ]),
        "ROOT_PRIVATE_SENTINEL" => Reply::done("coordinator reviewed the evidence"),
        "CHILD_ALPHA" => Reply::done("ALPHA_RESULT").blocked(&child_gate),
        "CHILD_BETA" => Reply::done("BETA_RESULT").blocked(&child_gate),
        _ => panic!("unexpected request actor {who:?}"),
    })
    .await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT_PRIVATE_SENTINEL"))
        .await
        .unwrap()
        .summary
        .id;
    until(|| {
        fixture.requests_for("CHILD_ALPHA").len() == 1
            && fixture.requests_for("CHILD_BETA").len() == 1
            && fixture.requests_for("ROOT_PRIVATE_SENTINEL").len() >= 2
    })
    .await;
    assert_eq!(state.get(&root).unwrap().summary.status, RunStatus::Running);
    let children: Vec<_> = state
        .list()
        .into_iter()
        .filter(|r| r.delegation.is_some())
        .collect();
    assert_eq!(children.len(), 2);
    assert!(
        children.iter().all(
            |r| r.status == RunStatus::Running && r.permission_mode == PermissionMode::ReadOnly
        )
    );
    for name in ["CHILD_ALPHA", "CHILD_BETA"] {
        let request = &fixture.requests_for(name)[0];
        assert!(request.to_string().contains("ROOT_PRIVATE_SENTINEL"));
        assert!(!request.to_string().contains(if name == "CHILD_ALPHA" {
            "CHILD_BETA"
        } else {
            "CHILD_ALPHA"
        }));
        let names: Vec<_> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"spawn_agent"));
        assert_eq!(
            names.contains(&"run_command"),
            state
                .workspace_review(&root)
                .await
                .unwrap()
                .sandbox_available
        );
        assert_eq!(
            names.contains(&"manage_command"),
            state
                .workspace_review(&root)
                .await
                .unwrap()
                .sandbox_available
        );
        assert!(!names.contains(&"call_mcp_tool"));
        assert!(names.contains(&"agent_control"));
    }
    gate.add_permits(2);
    let detail = settled(&state, &root).await;
    assert_eq!(
        detail.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        detail.error
    );
    let deliveries: Vec<_> = detail
        .events
        .iter()
        .filter(|event| event.r#type == "agent_message_received")
        .collect();
    assert_eq!(deliveries.len(), 2);
    assert!(
        deliveries
            .iter()
            .any(|event| event.detail.contains("ALPHA_RESULT"))
    );
    assert!(
        deliveries
            .iter()
            .any(|event| event.detail.contains("BETA_RESULT"))
    );
    assert!(
        state
            .list()
            .iter()
            .filter(|r| r.delegation.is_some())
            .all(|r| r.status == RunStatus::Completed)
    );
    let last = fixture
        .requests_for("ROOT_PRIVATE_SENTINEL")
        .pop()
        .unwrap()
        .to_string();
    assert!(last.contains("ALPHA_RESULT") && last.contains("BETA_RESULT"));
    state.shutdown().await;
}

#[tokio::test]
async fn messages_are_delivered_and_idle_children_resume_only_through_the_coordinator() {
    let fixture = Fixture::new(|who, index, request| {
        if who == "CHILD" { return Reply::done(if index == 0 { "FIRST_RESULT" } else { "SECOND_RESULT" }); }
        if index == 0 {
            Reply::tools(vec![spawn("child", "CHILD", &[])])
        } else if !request.to_string().contains("FIRST_RESULT") {
            Reply::done("waiting for initial child result")
        } else if !has_result(request, "send") {
            Reply::tools(vec![call("send", "agent_control", json!({"action":"send", "agent_id":spawned_id(request, "child"), "message":"FOLLOWUP_EVIDENCE"}))])
        } else if !has_result(request, "resume") {
            Reply::tools(vec![call("resume", "agent_control", json!({"action":"resume", "agent_id":spawned_id(request, "child"), "message":"Do the follow-up now."}))])
        } else {
            Reply::done("coordinator result")
        }
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    let child = state
        .list()
        .into_iter()
        .find(|r| r.delegation.is_some())
        .unwrap();
    assert_eq!(fixture.requests_for("CHILD").len(), 2);
    assert!(
        fixture.requests_for("CHILD")[1]
            .to_string()
            .contains("FOLLOWUP_EVIDENCE")
    );
    assert!(
        fixture.requests_for("CHILD")[1]
            .to_string()
            .contains("Do the follow-up now.")
    );
    assert!(
        state
            .get(&child.id)
            .unwrap()
            .events
            .iter()
            .any(|e| e.r#type == "agent_message_received")
    );
    assert!(
        state
            .resume(&child.id, Some("bypass coordinator".into()))
            .await
            .is_err()
    );
    assert!(
        state
            .input(&child.id, "bypass coordinator".into())
            .await
            .is_err()
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|e| e.r#type == "agent_message_received")
            .count(),
        2
    );
    state.shutdown().await;
}

#[tokio::test]
async fn cancelling_parent_stops_its_inflight_children_and_restart_keeps_relationships() {
    let gate = Arc::new(Semaphore::new(0));
    let child_gate = gate.clone();
    let fixture = Fixture::new(move |who, index, _| match (who, index) {
        ("ROOT", 0) => Reply::tools(vec![
            spawn("one", "CHILD_ONE", &[]),
            spawn("two", "CHILD_TWO", &[]),
        ]),
        ("ROOT", _) => Reply::done("waiting"),
        _ => Reply::done("late child response").blocked(&child_gate),
    })
    .await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    until(|| {
        fixture.requests_for("CHILD_ONE").len() == 1 && fixture.requests_for("CHILD_TWO").len() == 1
    })
    .await;
    tokio::time::timeout(Duration::from_secs(3), state.cancel(&root))
        .await
        .unwrap()
        .unwrap();
    assert!(
        state
            .list()
            .iter()
            .all(|r| r.status == RunStatus::Cancelled)
    );
    let ledger: Value = serde_json::from_slice(
        &fs::read(fixture.directory.join("state/collaboration.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        ledger["inboxes"][&root].as_array().unwrap().len(),
        2,
        "stopped parent retains unconsumed child outcomes"
    );
    state.shutdown().await;
    drop(state);
    let requests_before = fixture.provider.requests.lock().unwrap().len();
    let restored = fixture.state().await;
    fixture.configure(&restored).await;
    assert_eq!(restored.list().len(), 3);
    assert!(
        restored
            .list()
            .iter()
            .filter_map(|r| r.delegation.as_ref())
            .all(|d| d.parent_run_id == root && d.root_run_id == root)
    );
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert_eq!(
        fixture.provider.requests.lock().unwrap().len(),
        requests_before,
        "restoration must not start model work"
    );
    assert!(
        restored
            .list()
            .iter()
            .all(|r| r.status != RunStatus::Running)
    );
    let ledger: Value = serde_json::from_slice(
        &fs::read(fixture.directory.join("state/collaboration.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ledger["inboxes"][&root].as_array().unwrap().len(), 2);
    gate.add_permits(2);
    restored.shutdown().await;
}

#[tokio::test]
async fn interactive_runs_can_pass_twelve_tool_cycles_and_explicit_limits_pause_and_resume() {
    let fixture = Fixture::new(|who, index, _| {
        let tool_rounds = if who == "LONG" { 14 } else { 2 };
        if index < tool_rounds {
            Reply::tools(vec![call(
                &format!("read-{index}"),
                "read_file",
                json!({"path":format!("file-{index}.txt")}),
            )])
        } else {
            Reply::done("all evidence read")
        }
    })
    .await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model.clone(), "LONG"))
        .await
        .unwrap()
        .summary
        .id;
    let complete = settled(&state, &root).await;
    assert_eq!(
        complete.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        complete.error
    );
    assert_eq!(fixture.requests_for("LONG").len(), 15);
    assert_eq!(
        complete
            .events
            .iter()
            .filter(|e| e.r#type == "tool_call_succeeded")
            .count(),
        14
    );
    let limited = after_background_settles(
        || {
            let mut input = fixture.input(model.clone(), "LIMITED");
            input.execution_policy.max_steps = Some(2);
            state.create(input)
        },
        "此项目或其子目录已有正在执行的会话，请先停止或等待完成",
    )
    .await
    .summary
    .id;
    let paused = settled(&state, &limited).await;
    assert_eq!(paused.summary.status, RunStatus::Paused);
    assert!(paused.error.is_none());
    assert!(paused.events.iter().any(|e| e.r#type == "run_paused"));
    assert_eq!(fixture.requests_for("LIMITED").len(), 2);
    after_background_settles(
        || {
            state.resume(
                &limited,
                Some("Continue with the retained evidence.".into()),
            )
        },
        "请等待当前操作结束后继续",
    )
    .await;
    assert_eq!(
        settled(&state, &limited).await.summary.status,
        RunStatus::WaitingForInput
    );
    assert_eq!(fixture.requests_for("LIMITED").len(), 3);
    state.shutdown().await;
}

#[tokio::test]
async fn delegated_tools_cannot_expand_permissions_or_spawn_nested_agents() {
    let fixture = Fixture::new(|who, index, _| match (who, index) {
        ("ROOT", 0) => Reply::tools(vec![spawn("reject-write", "ESCALATED", &["file-0.txt"]), spawn("child", "CHILD", &[])]),
        ("CHILD", 0) => Reply::tools(vec![
            spawn("nested", "NESTED", &[]),
            call("shell", "run_command", json!({"command":"touch should-not-exist", "access":"workspace_write"})),
            call("mcp", "call_mcp_tool", json!({"server":"anything","name":"anything","arguments":{}})),
            call("patch", "apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: should-not-exist\n+no\n*** End Patch"})),
        ]),
        _ => Reply::done("permission checks complete"),
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert_eq!(state.list().len(), 2);
    assert!(result.events.iter().any(|e| e.r#type == "tool_call_failed"));
    let child = state
        .list()
        .into_iter()
        .find(|r| r.delegation.is_some())
        .unwrap();
    let detail = state.get(&child.id).unwrap();
    assert_eq!(detail.summary.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(
        detail
            .events
            .iter()
            .filter(|e| e.r#type == "tool_call_failed")
            .count(),
        4
    );
    assert!(!fixture.project.join("should-not-exist").exists());
    assert!(fixture.requests_for("NESTED").is_empty());
    assert!(fixture.requests_for("ESCALATED").is_empty());
    state.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn delegated_test_commands_inherit_only_explicit_root_session_approval() {
    let fixture = Fixture::new(|who, index, _| match (who, index) {
        ("ROOT", 0) => Reply::tools(vec![call("prepare", "run_command", json!({
            "command":"printf ROOT_READY", "access":"read_only", "timeout_seconds":5
        }))]),
        ("ROOT", 1) => Reply::tools(vec![spawn("tester", "TESTER", &[])]),
        ("TESTER", 0) => Reply::tools(vec![call("test", "run_command", json!({
            "command":"if printf escape > file-0.txt 2>/dev/null; then exit 81; fi; test \"$(cat file-0.txt)\" = 'Evidence 0' || exit 82; printf result > \"$TMPDIR/test-result\" || exit 83; printf CHILD_TEST_PASSED",
            "timeout_seconds":5
        }))]),
        _ => Reply::done("verification finished"),
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = state
                .workspace_review(&root)
                .await
                .unwrap()
                .approvals
                .into_iter()
                .next()
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("root command must ask for approval");
    assert_eq!(approval.access, agent_local::CommandAccess::ReadOnly);
    state
        .approve_command(
            &root,
            &approval.id,
            true,
            agent_local::CommandApprovalScope::Session,
        )
        .await
        .unwrap();
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    let child = state
        .list()
        .into_iter()
        .find(|run| run.delegation.is_some())
        .unwrap();
    let detail = state.get(&child.id).unwrap();
    assert!(
        !detail
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_failed")
    );
    let requests = fixture.requests_for("TESTER");
    assert_eq!(requests.len(), 2);
    let tool_result = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "test")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(tool_result.contains("CHILD_TEST_PASSED"), "{tool_result}");
    assert!(
        state
            .workspace_review(&child.id)
            .await
            .unwrap()
            .approvals
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("file-0.txt")).unwrap(),
        "Evidence 0\n"
    );
    state.revoke_command_grants(&child.id).await.unwrap();
    assert!(
        state
            .workspace_review(&root)
            .await
            .unwrap()
            .command_grants
            .is_empty()
    );
    state.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn approving_write_commands_while_review_is_polled_does_not_create_lock_conflicts() {
    const COMMANDS: usize = 12;
    let fixture = Fixture::new(|_, index, _| {
        if index == 0 {
            let mut patch = "*** Begin Patch\n".to_owned();
            for file in 0..64 {
                patch.push_str(&format!("*** Add File: review-{file}.txt\n+generated evidence\n"));
            }
            patch.push_str("*** End Patch");
            Reply::tools(vec![call("prepare-files", "apply_patch", json!({"patch":patch}))])
        } else if index <= COMMANDS {
            Reply::tools(vec![call(&format!("write-{index}"), "run_command", json!({
                "command":"printf x >> command-results.txt", "access":"workspace_write", "timeout_seconds":5
            }))])
        } else { Reply::done("writes verified") }
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let mut input = fixture.input(model, "ROOT");
    input.permission_mode = PermissionMode::WorkspaceWrite;
    let root = state.create(input).await.unwrap().summary.id;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = state
                .workspace_review(&root)
                .await
                .unwrap()
                .approvals
                .into_iter()
                .next()
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("write command must request approval");
    // Match the desktop: approval itself returns a fresh review, while other
    // requests keep polling the same root during command admission/completion.
    let mut pollers = Vec::new();
    for _ in 0..3 {
        let state = state.clone();
        let root = root.clone();
        pollers.push(tokio::spawn(async move {
            loop {
                state.workspace_review(&root).await.unwrap();
                tokio::task::yield_now().await;
            }
        }));
    }
    state
        .approve_command(
            &root,
            &approval.id,
            true,
            agent_local::CommandApprovalScope::Session,
        )
        .await
        .unwrap();
    let outcome = settled(&state, &root).await;
    for poller in pollers {
        poller.abort();
        let _ = poller.await;
    }
    assert_eq!(
        outcome.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        outcome.error
    );
    let failures: Vec<_> = outcome
        .events
        .iter()
        .filter(|event| event.r#type == "tool_call_failed")
        .collect();
    assert!(
        failures.is_empty(),
        "review polling caused command failures: {failures:?}"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("command-results.txt")).unwrap(),
        "x".repeat(COMMANDS)
    );
    state.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn parent_can_approve_and_stop_child_commands_without_exposing_other_roots() {
    let gate = Arc::new(Semaphore::new(0));
    let child_gate = gate.clone();
    let fixture = Fixture::new(move |who, index, _| match (who, index) {
        ("ROOT", 0) => Reply::tools(vec![spawn("tester", "TESTER", &[])]),
        ("TESTER", 0) => Reply::tools(vec![call(
            "long-test",
            "run_command",
            json!({
                "command":"printf CHILD_RUNNING; sleep 20", "yield_time_ms":0, "timeout_seconds":25
            }),
        )]),
        ("TESTER", _) => Reply::done("child finished").blocked(&child_gate),
        _ => Reply::done("coordinator finished"),
    })
    .await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let other_project = fixture.directory.join("unrelated-project");
    fs::create_dir(&other_project).unwrap();
    let mut unrelated = fixture.input(model.clone(), "OTHER");
    unrelated.workspace_path = Some(other_project.to_string_lossy().into_owned());
    let unrelated = state.create(unrelated).await.unwrap().summary.id;
    settled(&state, &unrelated).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval) = state
                .workspace_review(&root)
                .await
                .unwrap()
                .approvals
                .into_iter()
                .next()
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child approval must be visible in its parent");
    assert_eq!(approval.task_title.as_deref(), Some("TESTER"));
    assert!(
        state
            .workspace_review(&unrelated)
            .await
            .unwrap()
            .approvals
            .is_empty()
    );
    assert!(
        state
            .approve_command(
                &unrelated,
                &approval.id,
                true,
                agent_local::CommandApprovalScope::Session
            )
            .await
            .is_err()
    );
    assert_eq!(
        state.workspace_review(&root).await.unwrap().approvals.len(),
        1
    );
    state
        .approve_command(
            &root,
            &approval.id,
            true,
            agent_local::CommandApprovalScope::Once,
        )
        .await
        .unwrap();
    until(|| fixture.requests_for("TESTER").len() == 2).await;
    let review = state.workspace_review(&root).await.unwrap();
    let command = review
        .commands
        .iter()
        .find(|command| command.id == approval.id)
        .unwrap();
    assert_eq!(command.status, "running");
    assert_eq!(command.task_title.as_deref(), Some("TESTER"));
    assert!(review.command_grants.is_empty());
    assert!(!review.busy);
    assert!(state.stop_command(&unrelated, &approval.id).await.is_err());
    let review = state.stop_command(&root, &approval.id).await.unwrap();
    assert!(
        !review
            .commands
            .iter()
            .any(|command| command.id == approval.id)
    );
    gate.add_permits(1);
    settled(&state, &root).await;
    let child = state
        .list()
        .into_iter()
        .find(|run| {
            run.delegation
                .as_ref()
                .is_some_and(|delegation| delegation.root_run_id == root)
        })
        .unwrap();
    let review = state.workspace_review(&child.id).await.unwrap();
    assert_eq!(review.commands[0].status, "cancelled");
    assert!(review.commands[0].task_title.is_none());
    assert!(
        state
            .approve_command(
                &root,
                &approval.id,
                true,
                agent_local::CommandApprovalScope::Once
            )
            .await
            .is_err()
    );
    state.shutdown().await;
}

#[tokio::test]
async fn disjoint_file_leases_reject_conflicts_and_release_before_parent_writes() {
    let gate = Arc::new(Semaphore::new(0));
    let child_gate = gate.clone();
    let fixture = Fixture::new(move |who, index, request| {
        if who == "WRITER_A" || who == "WRITER_B" {
            let (path, text) = if who == "WRITER_A" { ("a.txt", "from child a") } else { ("b.txt", "from child b") };
            return if index == 0 {
                Reply::tools(vec![
                    call("own-file", "apply_patch", json!({"patch":format!("*** Begin Patch\n*** Add File: {path}\n+{text}\n*** End Patch")})),
                    call("outside-scope", "apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: unassigned.txt\n+not allowed\n*** End Patch"})),
                ]).blocked(&child_gate)
            } else { Reply::done(if who == "WRITER_A" { "WRITER_A_DONE" } else { "WRITER_B_DONE" }) };
        }
        if index == 0 {
            Reply::tools(vec![
                spawn("a", "WRITER_A", &["a.txt"]),
                spawn("b", "WRITER_B", &["b.txt"]),
                spawn("conflict", "CONFLICT", &["a.txt"]),
                spawn("escape", "ESCAPE", &["../escape.txt"]),
                call("parent-while-leased", "apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: a.txt\n+parent raced\n*** End Patch"})),
            ])
        } else if request.to_string().contains("WRITER_A_DONE") && request.to_string().contains("WRITER_B_DONE") && !has_result(request, "parent-after-release") {
            Reply::tools(vec![call("parent-after-release", "apply_patch", json!({"patch":"*** Begin Patch\n*** Update File: a.txt\n@@\n-from child a\n+parent verified\n*** End Patch"}))])
        } else { Reply::done("waiting for file owners or returning verified results") }
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let mut input = fixture.input(model, "ROOT");
    input.permission_mode = PermissionMode::WorkspaceWrite;
    let root = state.create(input).await.unwrap().summary.id;
    until(|| {
        fixture.requests_for("WRITER_A").len() == 1
            && fixture.requests_for("WRITER_B").len() == 1
            && fixture.requests_for("ROOT").len() >= 2
    })
    .await;
    assert_eq!(
        state.list().len(),
        3,
        "rejected leases do not create visible agents"
    );
    assert!(
        !fixture.project.join("a.txt").exists(),
        "parent cannot write a leased file"
    );
    assert!(!fixture.project.join("b.txt").exists());
    gate.add_permits(2);
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "parent verified\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("b.txt")).unwrap(),
        "from child b\n"
    );
    assert!(!fixture.project.join("unassigned.txt").exists());
    assert!(!fixture.directory.join("escape.txt").exists());
    assert!(fixture.requests_for("CONFLICT").is_empty());
    assert!(fixture.requests_for("ESCAPE").is_empty());
    assert_eq!(
        result
            .events
            .iter()
            .filter(|e| e.r#type == "tool_call_failed")
            .count(),
        3
    );
    for child in state.list().into_iter().filter(|r| r.delegation.is_some()) {
        let child_result = state.get(&child.id).unwrap();
        assert_eq!(child_result.summary.status, RunStatus::Completed);
        assert_eq!(
            child_result
                .events
                .iter()
                .filter(|e| e.r#type == "tool_call_failed")
                .count(),
            1
        );
    }
    state.shutdown().await;
}

#[tokio::test]
async fn concurrency_cap_rejects_extra_work_and_user_stopped_children_cannot_be_restarted() {
    let gate = Arc::new(Semaphore::new(0));
    let child_gate = gate.clone();
    let fixture = Fixture::new(move |who, index, request| {
        if who == "CHILD" { return Reply::done("not reached before cancellation").blocked(&child_gate); }
        if index == 0 {
            Reply::tools(vec![spawn("first", "CHILD", &[]), spawn("overflow", "OVERFLOW", &[])])
        } else if request.to_string().contains("stopped with status Cancelled") && !has_result(request, "restart-stopped") {
            Reply::tools(vec![call("restart-stopped", "agent_control", json!({"action":"resume","agent_id":spawned_id(request,"first"),"message":"Try again"}))])
        } else { Reply::done("waiting for the current delegated task") }
    }).await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let mut input = fixture.input(model, "ROOT");
    input.execution_policy.max_agents = Some(1);
    let root = state.create(input).await.unwrap().summary.id;
    until(|| fixture.requests_for("CHILD").len() == 1 && fixture.requests_for("ROOT").len() >= 2)
        .await;
    let children: Vec<_> = state
        .list()
        .into_iter()
        .filter(|r| r.delegation.is_some())
        .collect();
    assert_eq!(children.len(), 1);
    assert!(fixture.requests_for("OVERFLOW").is_empty());
    state.cancel(&children[0].id).await.unwrap();
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert_eq!(
        state.get(&children[0].id).unwrap().summary.status,
        RunStatus::Cancelled
    );
    assert_eq!(fixture.requests_for("CHILD").len(), 1);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|e| e.r#type == "tool_call_failed")
            .count(),
        2
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| e.r#type == "tool_call_failed" && e.detail.contains("用户已停止"))
    );
    gate.add_permits(1);
    state.shutdown().await;
}

#[tokio::test]
async fn stable_targets_and_host_identity_are_available_at_every_model_boundary() {
    fn result(request: &Value, id: &str) -> Value {
        let message = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
            .unwrap();
        serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
    }
    let fixture = Fixture::new(|who, index, request| {
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "system" || m["role"] == "developer")
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(system.contains("Host-provided collaboration identity"));
        if who == "ROOT" {
            return match index {
                0 => Reply::tools(vec![call(
                    "worker",
                    "spawn_agent",
                    json!({"task_name":"reviewer", "task":"CHILD"}),
                )]),
                1 => {
                    assert_eq!(result(request, "worker")["target"], "/root/reviewer");
                    Reply::tools(vec![call(
                        "inspect",
                        "agent_control",
                        json!({"action":"read", "target":"reviewer"}),
                    )])
                }
                _ => Reply::done("Verified child evidence"),
            };
        }
        assert!(system.contains("/root/reviewer"));
        match index {
            0 => Reply::tools(vec![call(
                "parent-read",
                "agent_control",
                json!({"action":"read","target":"parent","limit":1}),
            )]),
            1 => {
                assert_eq!(
                    result(request, "parent-read")["messages"][0]["text"],
                    "ROOT"
                );
                Reply::tools(vec![call(
                    "self-read",
                    "agent_control",
                    json!({"action":"read","target":"self","limit":1}),
                )])
            }
            _ => {
                assert_eq!(result(request, "self-read")["messages"][0]["text"], "CHILD");
                assert_ne!(
                    result(request, "parent-read")["agent_id"],
                    result(request, "self-read")["agent_id"]
                );
                Reply::done("Evidence from stable parent and self addresses")
            }
        }
    })
    .await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let root = state
        .create(fixture.input(model, "ROOT"))
        .await
        .unwrap()
        .summary
        .id;
    let result = settled(&state, &root).await;
    assert_eq!(
        result.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        result.error
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| e.detail.contains("Evidence from stable parent"))
    );
    state.shutdown().await;
}
