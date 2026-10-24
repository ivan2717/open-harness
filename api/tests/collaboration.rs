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
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
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
                supports_images: false,
            })
            .await
            .unwrap()
            .id
    }

    fn input(&self, model_id: String, prompt: &str) -> CreateRunInput {
        CreateRunInput {
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
async fn children_run_concurrently_with_fresh_context_and_parent_waits_for_durable_results() {
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
        assert!(!request.to_string().contains("ROOT_PRIVATE_SENTINEL"));
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
        assert!(!names.contains(&"run_command"));
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
            call("shell", "run_command", json!({"command":"touch should-not-exist"})),
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
