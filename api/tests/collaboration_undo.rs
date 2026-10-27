//! User undo uses the real provider transport, delegated tools, and disk journal.
use agent_local::PermissionMode;
use agent_providers::ProviderProtocol;
use api::{
    AppState,
    contracts::{ConfigureModel, CreateRunInput, RunStatus},
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

#[derive(Default)]
struct Provider {
    counts: Mutex<BTreeMap<String, usize>>,
    parent_gate: Option<Arc<Semaphore>>,
    other_gate: Option<Arc<Semaphore>>,
}

fn tool(name: &str, args: Value) -> Value {
    json!({"role":"assistant", "content":null, "tool_calls":[{
        "id":format!("call-{name}"), "type":"function",
        "function":{"name":name, "arguments":args.to_string()}
    }]})
}

async fn respond(State(provider): State<Arc<Provider>>, Json(request): Json<Value>) -> Json<Value> {
    let actor = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    let index = {
        let mut counts = provider.counts.lock().unwrap();
        let count = counts.entry(actor.to_owned()).or_default();
        let index = *count;
        *count += 1;
        index
    };
    let message = match (actor, index) {
        ("root", 0) => tool(
            "spawn_agent",
            json!({"task_name":"writer", "task":"writer", "write_paths":["a.txt", "b.txt"]}),
        ),
        ("writer", 0) => tool(
            "apply_patch",
            json!({"patch":"*** Begin Patch\n*** Update File: a.txt\n@@\n-original a\n+child a\n*** Update File: b.txt\n@@\n-original b\n+child b\n*** End Patch"}),
        ),
        ("followup", 0) => tool(
            "apply_patch",
            json!({"patch":"*** Begin Patch\n*** Update File: a.txt\n@@\n-user a\n+parent a\n*** End Patch"}),
        ),
        _ => {
            let gate = if actor == "root" {
                &provider.parent_gate
            } else if actor == "other" {
                &provider.other_gate
            } else {
                &None
            };
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            json!({"role":"assistant", "content":"Task complete."})
        }
    };
    let finish = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    Json(
        json!({"id":format!("reply-{actor}-{index}"), "model":"undo-fixture", "choices":[{"index":0,"message":message,"finish_reason":finish}], "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}}),
    )
}

struct Fixture {
    directory: PathBuf,
    project: PathBuf,
    state: AppState,
    model: String,
    provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(parent_gate: Option<Arc<Semaphore>>, other_gate: Option<Arc<Semaphore>>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-undo-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("a.txt"), "original a\n").unwrap();
        fs::write(project.join("b.txt"), "original b\n").unwrap();
        let provider = Arc::new(Provider {
            counts: Mutex::default(),
            parent_gate,
            other_gate,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let state = AppState::open_with_delay(directory.join("state"), None, Duration::ZERO)
            .await
            .unwrap();
        state
            .set_plugin_enabled("builtin.llm-wiki".into(), false)
            .await
            .unwrap();
        let model = state
            .configure_model(ConfigureModel {
                protocol: ProviderProtocol::ChatCompletions,
                base_url: endpoint,
                api_key: "local-test-only".into(),
                model: "undo-fixture".into(),
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
            .id;
        Self {
            directory,
            project,
            state,
            model,
            provider,
            server,
        }
    }

    async fn create(&self, prompt: &str) -> String {
        self.state
            .create(CreateRunInput {
                reasoning_effort: None,
                prompt: prompt.into(),
                model_id: self.model.clone(),
                files: Vec::new(),
                workspace_path: Some(self.project.display().to_string()),
                permission_mode: PermissionMode::WorkspaceWrite,
                execution_policy: Default::default(),
            })
            .await
            .unwrap()
            .summary
            .id
    }

    async fn child(&self) -> String {
        until(|| {
            self.state
                .list()
                .iter()
                .any(|run| run.delegation.is_some() && run.status != RunStatus::Running)
        })
        .await;
        let child = self
            .state
            .list()
            .into_iter()
            .find(|run| run.delegation.is_some())
            .unwrap();
        assert_eq!(child.status, RunStatus::Completed);
        assert_eq!(
            fs::read_to_string(self.project.join("a.txt")).unwrap(),
            "child a\n"
        );
        assert_eq!(
            fs::read_to_string(self.project.join("b.txt")).unwrap(),
            "child b\n"
        );
        child.id
    }

    async fn idle(&self, id: &str) {
        until(|| self.state.get(id).unwrap().summary.status != RunStatus::Running).await;
        // Status publication precedes completion delivery and lease release.
        // A no-op undo exercises the service's own family-idle admission gate.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let error = self
                    .state
                    .revert_file(id, "missing.txt")
                    .await
                    .unwrap_err()
                    .to_string();
                if error == "change not found" {
                    break;
                }
                assert!(
                    error.contains("等待当前操作"),
                    "unexpected admission error: {error}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixture did not reach expected state");
}

#[tokio::test]
async fn completed_child_undo_survives_restart_and_keeps_journals_separate() {
    let fixture = Fixture::new(None, None).await;
    let root = fixture.create("root").await;
    let child = fixture.child().await;
    fixture.idle(&root).await;
    assert!(
        fixture
            .state
            .workspace_review(&root)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    assert_eq!(
        fixture
            .state
            .workspace_review(&child)
            .await
            .unwrap()
            .changes
            .len(),
        2
    );
    fixture.state.shutdown().await;

    let reopened = AppState::open_with_delay(fixture.directory.join("state"), None, Duration::ZERO)
        .await
        .unwrap();
    let review = reopened.revert_file(&child, "./a.txt").await.unwrap();
    assert_eq!(review.changes.len(), 1);
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "original a\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("b.txt")).unwrap(),
        "child b\n"
    );
    assert_eq!(
        reopened
            .revert_file(&child, "a.txt")
            .await
            .unwrap_err()
            .to_string(),
        "change not found"
    );
    assert!(
        reopened
            .revert_file(&child, "b.txt")
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("b.txt")).unwrap(),
        "original b\n"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn child_undo_waits_for_the_entire_family() {
    let gate = Arc::new(Semaphore::new(0));
    let fixture = Fixture::new(Some(gate.clone()), None).await;
    let root = fixture.create("root").await;
    let child = fixture.child().await;
    assert_eq!(
        fixture.state.get(&root).unwrap().summary.status,
        RunStatus::Running
    );
    let error = fixture
        .state
        .revert_file(&child, "a.txt")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("此任务及其子任务"), "{error}");
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "child a\n"
    );
    gate.add_permits(10);
    fixture.idle(&root).await;
    fixture.state.revert_file(&child, "a.txt").await.unwrap();
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn child_undo_refuses_another_active_root_in_the_same_workspace() {
    let gate = Arc::new(Semaphore::new(0));
    let fixture = Fixture::new(None, Some(gate.clone())).await;
    let root = fixture.create("root").await;
    let child = fixture.child().await;
    fixture.idle(&root).await;
    let other = fixture.create("other").await;
    until(|| {
        fixture
            .provider
            .counts
            .lock()
            .unwrap()
            .contains_key("other")
    })
    .await;
    let error = fixture
        .state
        .revert_file(&child, "a.txt")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("此项目或其子目录已有正在执行"), "{error}");
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "child a\n"
    );
    gate.add_permits(10);
    fixture.idle(&other).await;
    fixture.state.revert_file(&child, "a.txt").await.unwrap();
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn failed_child_undo_preserves_later_changes_and_releases_its_lease() {
    let fixture = Fixture::new(None, None).await;
    let root = fixture.create("root").await;
    let child = fixture.child().await;
    fixture.idle(&root).await;
    fs::write(fixture.project.join("a.txt"), "user a\n").unwrap();
    let error = fixture
        .state
        .revert_file(&child, "a.txt")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refusing to overwrite external changes"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "user a\n"
    );

    // A different owner can now edit the same path: the failed undo must not
    // retain the child's temporary lease, and no journal is merged into it.
    let next = fixture.create("followup").await;
    fixture.idle(&next).await;
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "parent a\n"
    );
    assert!(fixture.state.revert_file(&child, "a.txt").await.is_err());
    fixture.state.revert_file(&next, "a.txt").await.unwrap();
    assert_eq!(
        fs::read_to_string(fixture.project.join("a.txt")).unwrap(),
        "user a\n"
    );
    fixture.state.shutdown().await;
}
