use agent_local::PermissionMode;
use agent_providers::ProviderProtocol;
use api::{
    AppState,
    contracts::{ConfigureModel, CreateRunInput, RunStatus},
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

static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    project: PathBuf,
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "open-harness-local-flow-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("AGENTS.md"),
            "Keep the existing user's changes. Use focused patches.",
        )
        .unwrap();
        fs::write(
            project.join("answer.txt"),
            "user's existing edit\nanswer = 41\n",
        )
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            directory,
            project,
            endpoint,
            requests,
            server,
        }
    }

    async fn state(&self) -> AppState {
        AppState::open_with_delay(self.directory.join("state"), None, Duration::ZERO)
            .await
            .unwrap()
    }

    async fn configure(&self, state: &AppState) -> String {
        state
            .configure_model(ConfigureModel {
                supports_images: false,
                protocol: ProviderProtocol::ChatCompletions,
                base_url: self.endpoint.clone(),
                api_key: "local-test-only".into(),
                model: "local-tool-fixture".into(),
                context_window: 128_000,
                max_output_tokens: 4096,
                reasoning_mode: "off".into(),
                reasoning_effort: "high".into(),
            })
            .await
            .unwrap()
            .id
    }

    fn input(&self, model_id: String, permission_mode: PermissionMode) -> CreateRunInput {
        CreateRunInput {
            execution_policy: Default::default(),
            prompt: "Fix answer.txt and preserve my existing changes.".into(),
            files: Vec::new(),
            model_id,
            workspace_path: Some(self.project.display().to_string()),
            permission_mode,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn respond(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let mut requests = requests.lock().unwrap();
    let index = requests.len();
    requests.push(request);
    let call = match index {
        0 => Some(("read_file", json!({"path":"answer.txt"}))),
        1 => Some((
            "apply_patch",
            json!({"patch":"*** Begin Patch\n*** Update File: answer.txt\n@@\n user's existing edit\n-answer = 41\n+answer = 42\n*** End Patch"}),
        )),
        _ => None,
    };
    let (message, finish) = if let Some((name, arguments)) = call {
        (
            json!({"role":"assistant","content":null,"tool_calls":[{
                "id":format!("call-{index}"),"type":"function", "function":{"name":name,"arguments":arguments.to_string()}
            }]}),
            "tool_calls",
        )
    } else {
        (
            json!({"role":"assistant","content":"Reviewed the local operation results."}),
            "stop",
        )
    };
    Json(
        json!({"id":format!("response-{index}"),"model":"local-tool-fixture","choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":{"prompt_tokens":1200,"completion_tokens":50,"total_tokens":1250}}),
    )
}

async fn settled(state: &AppState, id: &str) -> api::contracts::RunDetail {
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

#[tokio::test]
async fn real_provider_transport_edits_reviews_restores_and_conditionally_reverts() {
    let fixture = Fixture::new().await;
    let state = fixture.state().await;
    let model_id = fixture.configure(&state).await;
    let created = state
        .create(fixture.input(model_id, PermissionMode::WorkspaceWrite))
        .await
        .unwrap();
    let id = created.summary.id;
    let complete = settled(&state, &id).await;
    assert_eq!(
        complete.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        complete.error
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("answer.txt")).unwrap(),
        "user's existing edit\nanswer = 42\n"
    );
    let review = state.workspace_review(&id).await.unwrap();
    assert_eq!(review.changes.len(), 1);
    assert!(review.changes[0].revertible);
    assert_eq!(
        review.changes[0].before.as_deref(),
        Some("user's existing edit\nanswer = 41\n")
    );
    let mut tool_names: Vec<String> = fixture.requests.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    tool_names.sort();
    let mut expected_tools = vec![
        "agent_control",
        "apply_patch",
        "list_files",
        "read_file",
        "search_files",
        "session_memory",
        "spawn_agent",
    ];
    if review.sandbox_available {
        expected_tools.push("run_command");
    }
    expected_tools.sort();
    assert_eq!(tool_names, expected_tools);
    let first_instructions = fixture.requests.lock().unwrap()[0]["messages"][0].clone();
    assert!(
        first_instructions
            .to_string()
            .contains("Use focused patches")
    );

    // Reopening must preserve the exact original instructions/provider prefix,
    // the journal, and the model binding, even when project guidance changed.
    fs::write(
        fixture.project.join("AGENTS.md"),
        "Different guidance for NEW conversations.",
    )
    .unwrap();
    state.shutdown().await;
    drop(state);
    let reopened = fixture.state().await;
    fixture.configure(&reopened).await;
    reopened
        .resume(
            &id,
            Some("Check the recorded results; do not repeat edits.".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        settled(&reopened, &id).await.summary.status,
        RunStatus::WaitingForInput
    );
    assert_eq!(
        fixture.requests.lock().unwrap().last().unwrap()["messages"][0],
        first_instructions
    );
    assert_eq!(
        reopened.workspace_review(&id).await.unwrap().changes.len(),
        1
    );

    // An external change blocks undo, rather than discarding the user's work.
    fs::write(
        fixture.project.join("answer.txt"),
        "new user edit after the agent\n",
    )
    .unwrap();
    assert!(reopened.revert_file(&id, "answer.txt").await.is_err());
    assert_eq!(
        fs::read_to_string(fixture.project.join("answer.txt")).unwrap(),
        "new user edit after the agent\n"
    );
    fs::write(
        fixture.project.join("answer.txt"),
        "user's existing edit\nanswer = 42\n",
    )
    .unwrap();
    let review = reopened.revert_file(&id, "answer.txt").await.unwrap();
    assert!(review.changes.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.project.join("answer.txt")).unwrap(),
        "user's existing edit\nanswer = 41\n"
    );
}

#[tokio::test]
async fn readonly_binding_rejects_model_writes_and_missing_projects_do_not_break_history() {
    let fixture = Fixture::new().await;
    let state = fixture.state().await;
    let model_id = fixture.configure(&state).await;
    let created = state
        .create(fixture.input(model_id, PermissionMode::ReadOnly))
        .await
        .unwrap();
    let id = created.summary.id;
    let detail = settled(&state, &id).await;
    assert_eq!(detail.summary.status, RunStatus::WaitingForInput);
    assert!(
        detail
            .events
            .iter()
            .any(|event| event.r#type == "tool_call_failed")
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("answer.txt")).unwrap(),
        "user's existing edit\nanswer = 41\n"
    );
    state.shutdown().await;
    drop(state);
    fs::rename(&fixture.project, fixture.directory.join("moved-project")).unwrap();
    let reopened = fixture.state().await;
    fixture.configure(&reopened).await;
    assert_eq!(
        reopened.get(&id).unwrap().messages.len(),
        detail.messages.len()
    );
    assert!(reopened.resume(&id, None).await.is_err());
    assert!(reopened.workspace_review(&id).await.is_err());
}

#[tokio::test]
async fn overlapping_workspace_runs_and_undo_while_running_are_rejected() {
    let fixture = Fixture::new().await;
    let state = fixture.state().await;
    let model_id = fixture.configure(&state).await;
    let first = state
        .create(fixture.input(model_id.clone(), PermissionMode::WorkspaceWrite))
        .await
        .unwrap();
    assert!(
        state
            .create(fixture.input(model_id.clone(), PermissionMode::WorkspaceWrite))
            .await
            .is_err()
    );
    let nested = fixture.project.join("nested");
    fs::create_dir(&nested).unwrap();
    let mut input = fixture.input(model_id, PermissionMode::WorkspaceWrite);
    input.workspace_path = Some(nested.display().to_string());
    assert!(state.create(input).await.is_err());
    assert!(
        state
            .revert_file(&first.summary.id, "answer.txt")
            .await
            .is_err()
    );
    state.cancel(&first.summary.id).await.unwrap();
}

#[tokio::test]
async fn project_bindings_reject_service_storage_and_demo_without_breaking_legacy_input() {
    let fixture = Fixture::new().await;
    let state = fixture.state().await;
    let model_id = fixture.configure(&state).await;
    let mut input = fixture.input(model_id.clone(), PermissionMode::WorkspaceWrite);
    input.workspace_path = Some(fixture.directory.display().to_string());
    assert!(state.create(input).await.is_err());
    let mut input = fixture.input(model_id, PermissionMode::WorkspaceWrite);
    input.workspace_path = Some("relative-directory".into());
    assert!(state.create(input).await.is_err());
    assert!(
        state
            .create(fixture.input("local-demo".into(), PermissionMode::ReadOnly))
            .await
            .is_err()
    );
    let legacy: CreateRunInput =
        serde_json::from_value(json!({"prompt":"legacy","model_id":"local-demo"})).unwrap();
    assert!(legacy.workspace_path.is_none());
    assert_eq!(legacy.permission_mode, PermissionMode::ReadOnly);
}

#[tokio::test]
async fn snapshot_failure_does_not_lose_the_authoritative_resume_event() {
    let fixture = Fixture::new().await;
    let state = fixture.state().await;
    let model = fixture.configure(&state).await;
    let created = state
        .create(fixture.input(model, PermissionMode::ReadOnly))
        .await
        .unwrap();
    let id = created.summary.id;
    settled(&state, &id).await;
    state.cancel(&id).await.unwrap();
    let blocked_snapshot = fixture
        .directory
        .join("state/runs")
        .join(format!("{id}.json.tmp"));
    fs::create_dir(&blocked_snapshot).unwrap();
    // The recovery event commits before context observation hits the unwritable
    // snapshot. That turn fails, but its durable recovery history is retained.
    state
        .resume(&id, Some("Continue after cancellation.".into()))
        .await
        .unwrap();
    assert_eq!(settled(&state, &id).await.summary.status, RunStatus::Failed);
    fs::remove_dir(&blocked_snapshot).unwrap();
    state.shutdown().await;
    drop(state);
    let restored = fixture.state().await;
    fixture.configure(&restored).await;
    assert!(
        restored
            .get(&id)
            .unwrap()
            .events
            .iter()
            .any(|event| event.r#type == "run_resumed")
    );
    restored
        .resume(
            &id,
            Some("Continue from the persisted recovery event.".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        settled(&restored, &id).await.summary.status,
        RunStatus::WaitingForInput
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("answer.txt")).unwrap(),
        "user's existing edit\nanswer = 41\n"
    );
}
