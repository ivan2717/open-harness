use super::*;
use agent_core::agent::RunEvent;
use agent_providers::ProviderProtocol;
use agent_runtime::{EventStore, ModelResponse};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};

struct Fixture {
    directory: PathBuf,
    state: AppState,
    original: ModelInfo,
}

fn config(protocol: ProviderProtocol, url: &str, name: &str) -> ConfigureModel {
    ConfigureModel {
        protocol,
        base_url: url.into(),
        api_key: "session-model-fixture-key".into(),
        model: name.into(),
        context_window: 32_000,
        max_output_tokens: 16_384,
        reasoning_mode: "off".into(),
        reasoning_effort: "medium".into(),
        reasoning_capability: Some(serde_json::from_value(json!({
            "source": "manual",
            "adapter": if protocol == ProviderProtocol::AnthropicMessages { "anthropic_budget" } else { "effort" },
            "options": if protocol == ProviderProtocol::AnthropicMessages {
                json!([{"id":"low","budget_tokens":1024},{"id":"medium","budget_tokens":4096},{"id":"high","budget_tokens":8192}])
            } else {
                json!([{"id":"low","effort":"low"},{"id":"medium","effort":"medium"},{"id":"high","effort":"high"}])
            }
        })).unwrap()),
        reasoning_default: None,
        supports_images: false,
    }
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-session-model-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let original = state
            .configure_model(config(
                ProviderProtocol::ChatCompletions,
                "http://127.0.0.1:1/v1",
                "original",
            ))
            .await
            .unwrap();
        Self {
            directory,
            state,
            original,
        }
    }

    async fn conversation(&self, id: &str) -> Arc<RunEntry> {
        let entry = self
            .state
            .make_entry(
                RunDetail {
                    summary: RunSummary {
                        reasoning_settings_version: 1,
                        id: id.into(),
                        revision: 0,
                        title: "Existing conversation".into(),
                        prompt: "Remember my original goal".into(),
                        status: RunStatus::Running,
                        model_id: self.original.id.clone(),
                        reasoning_effort: None,
                        model_history_through: None,
                        delegation: None,
                        execution_policy: Default::default(),
                        compacting: false,
                        workspace_path: None,
                        execution_directory: None,
                        permission_mode: Default::default(),
                        created_at: time::now(),
                        updated_at: time::now(),
                    },
                    messages: vec![],
                    events: vec![],
                    context: Some(initial_context()),
                    error: None,
                },
                0,
            )
            .unwrap();
        let mut run = entry
            .runtime
            .start_run(&entry.agent, id, "Remember my original goal")
            .await
            .unwrap();
        run.begin_step().unwrap();
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::ModelResponseReceived {
                    content: "The original result".into(),
                    response: Some(ModelResponse::final_answer("The original result".into())),
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: None,
                },
            )
            .await
            .unwrap();
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::InputRequested {
                    prompt: "The original result".into(),
                },
            )
            .await
            .unwrap();
        self.state
            .inner
            .runs
            .write()
            .unwrap()
            .insert(id.into(), entry.clone());
        entry
    }

    async fn change(
        &self,
        id: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Result<RunDetail, ServiceError> {
        self.state
            .update_run_model(
                id,
                UpdateRunModelInput {
                    model_id: model.into(),
                    reasoning_effort: effort.map(str::to_owned),
                },
            )
            .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn switching_one_conversation_preserves_history_subscriptions_and_restores_binding() {
    let fixture = Fixture::new().await;
    fixture.conversation("selected").await;
    fixture.conversation("other").await;
    let mut target = config(
        ProviderProtocol::Responses,
        "http://127.0.0.1:2/v1",
        "replacement",
    );
    target.context_window = 1_000_000;
    let replacement = fixture.state.configure_model(target).await.unwrap();
    let before = fixture.state.get("selected").unwrap();
    let store = JsonlEventStore::new(fixture.directory.join("events"));
    let history_before = serde_json::to_value(
        store
            .load_for_runtime("selected")
            .await
            .unwrap()
            .unwrap()
            .events,
    )
    .unwrap();
    let mut subscriber = fixture.state.subscribe("selected").unwrap();
    let after = fixture
        .change("selected", &replacement.id, Some("high"))
        .await
        .unwrap();
    assert!(subscriber.has_changed().unwrap());
    assert_eq!(
        subscriber.borrow_and_update().summary.model_id,
        replacement.id
    );
    assert_eq!(after.summary.reasoning_effort.as_deref(), Some("high"));
    assert!(after.summary.model_history_through.is_some());
    assert_eq!(after.summary.id, before.summary.id);
    assert_eq!(after.summary.status, before.summary.status);
    assert_eq!(
        serde_json::to_value(after.messages).unwrap(),
        serde_json::to_value(before.messages).unwrap()
    );
    assert_eq!(
        serde_json::to_value(after.events).unwrap(),
        serde_json::to_value(before.events).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            store
                .load_for_runtime("selected")
                .await
                .unwrap()
                .unwrap()
                .events
        )
        .unwrap(),
        history_before
    );
    assert_eq!(after.context.unwrap().window_tokens, 1_000_000);
    assert_eq!(
        fixture.state.get("other").unwrap().summary.model_id,
        fixture.original.id
    );
    assert!(
        fixture
            .state
            .config()
            .models
            .iter()
            .any(|m| m.id == fixture.original.id)
    );
    assert_eq!(replacement.reasoning.unwrap().default_effort, None);
    let revision = after.summary.revision;
    let boundary = after.summary.model_history_through;
    let same = fixture
        .change("selected", &replacement.id, Some("high"))
        .await
        .unwrap();
    assert_eq!(same.summary.revision, revision);
    assert_eq!(same.summary.model_history_through, boundary);

    fixture.state.shutdown().await;
    let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let mut target = config(
        ProviderProtocol::Responses,
        "http://127.0.0.1:2/v1",
        "replacement",
    );
    target.context_window = 1_000_000;
    restored.configure_model(target).await.unwrap();
    let restored_detail = restored.get("selected").unwrap();
    assert_eq!(restored_detail.summary.model_id, replacement.id);
    assert_eq!(
        restored_detail.summary.reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(restored_detail.summary.model_history_through, boundary);
    let identity = restored
        .entry("selected")
        .unwrap()
        .runtime
        .model()
        .provider_context_identity()
        .cloned()
        .unwrap();
    assert!(identity.format.contains("high"));
    restored.shutdown().await;
}

#[tokio::test]
async fn invalid_and_busy_changes_leave_the_original_binding_untouched() {
    let fixture = Fixture::new().await;
    let entry = fixture.conversation("selected").await;
    for blocker in ["running", "compacting", "background"] {
        match blocker {
            "running" => entry
                .snapshot
                .update(|d| d.summary.status = RunStatus::Running)
                .unwrap(),
            "compacting" => entry
                .snapshot
                .update(|d| d.summary.compacting = true)
                .unwrap(),
            _ => entry.background.store(true, Ordering::Release),
        }
        assert!(matches!(
            fixture
                .change("selected", &fixture.original.id, Some("low"))
                .await,
            Err(ServiceError::Conflict(_))
        ));
        assert!(
            fixture
                .state
                .get("selected")
                .unwrap()
                .summary
                .reasoning_effort
                .is_none()
        );
        entry
            .snapshot
            .update(|d| {
                d.summary.status = RunStatus::WaitingForInput;
                d.summary.compacting = false;
            })
            .unwrap();
        entry.background.store(false, Ordering::Release);
    }
    assert!(matches!(
        fixture.change("selected", "missing", None).await,
        Err(ServiceError::BadRequest(_))
    ));
    assert!(matches!(
        fixture
            .change("selected", &fixture.original.id, Some("ultra"))
            .await,
        Err(ServiceError::BadRequest(_))
    ));
    assert!(matches!(
        fixture.change("selected", MODEL_ID, None).await,
        Err(ServiceError::BadRequest(_))
    ));
    let before = serde_json::to_value(fixture.state.get("selected").unwrap()).unwrap();
    let temporary = fixture.directory.join("runs/selected.json.tmp");
    fs::create_dir(&temporary).unwrap();
    assert!(matches!(
        fixture
            .change("selected", &fixture.original.id, Some("high"))
            .await,
        Err(ServiceError::Internal(_))
    ));
    assert_eq!(
        serde_json::to_value(fixture.state.get("selected").unwrap()).unwrap(),
        before
    );
    assert_eq!(
        fixture
            .state
            .entry("selected")
            .unwrap()
            .runtime
            .model()
            .provider_context_identity(),
        entry.runtime.model().provider_context_identity()
    );
    fs::remove_dir(&temporary).unwrap();
    fixture
        .change("selected", &fixture.original.id, Some("high"))
        .await
        .unwrap();
    let reset = fixture
        .change("selected", &fixture.original.id, None)
        .await
        .unwrap();
    assert!(reset.summary.reasoning_effort.is_none());
    assert_eq!(
        fixture
            .state
            .entry("selected")
            .unwrap()
            .runtime
            .model()
            .provider_context_identity(),
        entry.runtime.model().provider_context_identity()
    );
}

#[tokio::test]
async fn unavailable_source_can_be_replaced_and_route_rejects_unknown_fields() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fixture = Fixture::new().await;
    fixture.conversation("selected").await;
    fixture
        .state
        .inner
        .models
        .write()
        .unwrap()
        .remove(&fixture.original.id);
    let target = fixture
        .state
        .configure_model(config(
            ProviderProtocol::Responses,
            "http://127.0.0.1:2/v1",
            "new-provider",
        ))
        .await
        .unwrap();
    let router = crate::app(fixture.state.clone());
    for (extra, status) in [
        (true, StatusCode::UNPROCESSABLE_ENTITY),
        (false, StatusCode::OK),
    ] {
        let mut body = json!({"model_id":target.id,"reasoning_effort":"medium"});
        if extra {
            body["api_key"] = json!("must-not-be-accepted");
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/runs/selected/model")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if !extra {
            let bytes = to_bytes(response.into_body(), 100_000).await.unwrap();
            let detail: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(detail["model_id"], target.id);
            assert_eq!(detail["reasoning_effort"], "medium");
            assert!(!String::from_utf8_lossy(&bytes).contains("session-model-fixture-key"));
        }
    }
}

#[tokio::test]
async fn newly_delegated_agents_inherit_effort_but_cannot_be_changed_directly() {
    let fixture = Fixture::new().await;
    fixture.conversation("parent").await;
    fixture
        .change("parent", &fixture.original.id, Some("high"))
        .await
        .unwrap();
    fixture
        .state
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|d| d.summary.status = RunStatus::Running)
        .unwrap();
    fixture
        .state
        .collaboration_call(
            "parent",
            agent_runtime::ToolCall {
                id: "spawn-test".into(),
                name: "spawn_agent".into(),
                arguments:
                    json!({"task_name":"child", "task":"Inspect this fixture", "write_paths":[]})
                        .to_string(),
            },
        )
        .await
        .unwrap();
    let child = fixture
        .state
        .list()
        .into_iter()
        .find(|r| r.delegation.is_some())
        .unwrap();
    assert_eq!(child.model_id, fixture.original.id);
    assert_eq!(child.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(child.model_history_through, None);
    assert!(
        fixture
            .state
            .entry(&child.id)
            .unwrap()
            .runtime
            .model()
            .provider_context_identity()
            .unwrap()
            .format
            .contains("high")
    );
    assert!(matches!(
        fixture
            .change(&child.id, &fixture.original.id, Some("low"))
            .await,
        Err(ServiceError::Conflict(_))
    ));
    fixture.state.shutdown().await;
}

async fn fake_model(
    State((requests, response)): State<(Arc<std::sync::Mutex<Vec<Value>>>, Value)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    requests.lock().unwrap().push(body);
    Json(response)
}

#[tokio::test]
async fn next_request_uses_selected_model_and_reasoning_in_all_three_protocols() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        let fixture = Fixture::new().await;
        fixture.conversation("selected").await;
        let (endpoint, response) = match protocol {
            ProviderProtocol::ChatCompletions => (
                "chat/completions",
                json!({"id":"chat-fixture", "choices":[{"message":{"role":"assistant","content":"New response"},"finish_reason":"stop"}]}),
            ),
            ProviderProtocol::Responses => (
                "responses",
                json!({"id":"response-fixture", "status":"completed", "output":[{"id":"m1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"New response"}]}]}),
            ),
            ProviderProtocol::AnthropicMessages => (
                "messages",
                json!({"id":"message-fixture", "type":"message", "role":"assistant", "content":[{"type":"text","text":"New response"}], "stop_reason":"end_turn"}),
            ),
        };
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(&format!("/v1/{endpoint}"), post(fake_model))
                    .with_state((requests.clone(), response)),
            )
            .into_future(),
        );
        let target = fixture
            .state
            .configure_model(config(protocol, &url, "chosen-model"))
            .await
            .unwrap();
        fixture
            .change("selected", &target.id, Some("low"))
            .await
            .unwrap();
        fixture
            .state
            .input("selected", "Continue from my original goal".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let entry = fixture.state.entry("selected").unwrap();
                if !entry.background.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let detail = fixture.state.get("selected").unwrap();
        assert_eq!(
            detail.summary.status,
            RunStatus::WaitingForInput,
            "{protocol:?}: {:?}",
            detail.error
        );
        {
            let sent = requests.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["model"], "chosen-model");
            assert!(sent[0].to_string().contains("Remember my original goal"));
            assert!(sent[0].to_string().contains("The original result"));
            match protocol {
                ProviderProtocol::ChatCompletions => assert_eq!(sent[0]["reasoning_effort"], "low"),
                ProviderProtocol::Responses => assert_eq!(sent[0]["reasoning"]["effort"], "low"),
                ProviderProtocol::AnthropicMessages => {
                    assert_eq!(sent[0]["thinking"]["budget_tokens"], 1024)
                }
            }
        }
        fixture.state.shutdown().await;
        server.abort();
    }
}

async fn wait_for_model_turn(state: &AppState, id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while state.entry(id).unwrap().background.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let detail = state.get(id).unwrap();
    assert_eq!(
        detail.summary.status,
        RunStatus::WaitingForInput,
        "{:?}",
        detail.error
    );
}

#[tokio::test]
async fn pre_capability_selection_restores_and_continues_without_expanding_protocol_preset() {
    let fixture = Fixture::new().await;
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, Router::new().route("/v1/chat/completions", post(fake_model)).with_state((requests.clone(), json!({"id":"upgrade-fixture", "choices":[{"message":{"role":"assistant","content":"Preserved prior reply"},"finish_reason":"stop"}]})))).into_future());
    let manual = fixture
        .state
        .configure_model(config(
            ProviderProtocol::ChatCompletions,
            &url,
            "upgrade-model",
        ))
        .await
        .unwrap();
    let created = fixture.state.create(serde_json::from_value(json!({"prompt":"Remember the legacy goal", "model_id":manual.id, "reasoning_effort":"high"})).unwrap()).await.unwrap();
    let id = created.summary.id;
    wait_for_model_turn(&fixture.state, &id).await;
    fixture.state.shutdown().await;

    let legacy_input = || {
        let mut input = config(ProviderProtocol::ChatCompletions, &url, "upgrade-model");
        input.reasoning_capability = None;
        input
    };
    let legacy_id = crate::model::configure(legacy_input()).unwrap().info.id;
    let path = fixture.directory.join("runs").join(format!("{id}.json"));
    let mut snapshot: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    snapshot["model_id"] = json!(legacy_id);
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("reasoning_settings_version");
    fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    // The prior real response already uses the exact pre-upgrade Effort identity.
    for round in 0..2 {
        let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let registered = restored.configure_model(legacy_input()).await.unwrap();
        assert_eq!(registered.id, legacy_id);
        assert_eq!(
            registered.reasoning.unwrap().supported_efforts,
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            restored
                .get(&id)
                .unwrap()
                .summary
                .reasoning_settings_version,
            0
        );
        assert_eq!(
            restored
                .get(&id)
                .unwrap()
                .summary
                .reasoning_effort
                .as_deref(),
            Some("high")
        );
        assert!(restored.create(serde_json::from_value(json!({"prompt":"New unsupported choice", "model_id":legacy_id, "reasoning_effort":"ultra"})).unwrap()).await.is_err());
        assert!(
            restored
                .update_run_model(
                    &id,
                    UpdateRunModelInput {
                        model_id: legacy_id.clone(),
                        reasoning_effort: Some("ultra".into())
                    }
                )
                .await
                .is_err()
        );
        restored
            .input(&id, format!("Continue preserved conversation {round}"))
            .await
            .unwrap();
        wait_for_model_turn(&restored, &id).await;
        {
            let sent = requests.lock().unwrap();
            assert_eq!(sent.last().unwrap()["reasoning_effort"], "high");
            assert!(
                sent.last()
                    .unwrap()
                    .to_string()
                    .contains("Preserved prior reply")
            );
        }
        assert_eq!(
            restored
                .get(&id)
                .unwrap()
                .summary
                .reasoning_settings_version,
            0
        );
        restored.shutdown().await;
    }
    let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    restored.configure_model(legacy_input()).await.unwrap();
    restored
        .update_run_model(
            &id,
            UpdateRunModelInput {
                model_id: legacy_id,
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        restored
            .get(&id)
            .unwrap()
            .summary
            .reasoning_settings_version,
        1
    );
    restored
        .input(&id, "Use the provider default now".into())
        .await
        .unwrap();
    wait_for_model_turn(&restored, &id).await;
    assert!(
        requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );
    restored.shutdown().await;
    server.abort();
}

async fn wait_for_child_turn(state: &AppState, id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while state.entry(id).unwrap().background.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let detail = state.get(id).unwrap();
    assert_eq!(
        detail.summary.status,
        RunStatus::Completed,
        "{:?}",
        detail.error
    );
}

#[tokio::test]
async fn resumed_child_follows_parent_model_without_replaying_foreign_reasoning() {
    let fixture = Fixture::new().await;
    let source_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let target_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let source = Router::new()
        .route("/v1/chat/completions", post(fake_model))
        .with_state((source_requests.clone(), json!({
            "id":"old-child-response",
            "choices":[{"message":{"role":"assistant","content":"Verified result from the previous child turn","reasoning_content":"private-source-reasoning-marker"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":30,"completion_tokens_details":{"reasoning_tokens":10}}
        })));
    let target = Router::new()
        .route("/v1/responses", post(fake_model))
        .with_state((target_requests.clone(), json!({
            "id":"new-child-response", "status":"completed",
            "output":[{"id":"m1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Verified follow-up result"}]}]
        })));
    let server = tokio::spawn(axum::serve(listener, source.merge(target)).into_future());
    let original = fixture
        .state
        .configure_model(config(
            ProviderProtocol::ChatCompletions,
            &url,
            "source-model",
        ))
        .await
        .unwrap();
    let replacement = fixture
        .state
        .configure_model(config(ProviderProtocol::Responses, &url, "target-model"))
        .await
        .unwrap();
    fixture.conversation("parent").await;
    fixture
        .change("parent", &original.id, Some("low"))
        .await
        .unwrap();
    fixture
        .state
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::Running)
        .unwrap();
    let spawned: Value = serde_json::from_str(&fixture.state.collaboration_call("parent", agent_runtime::ToolCall {
        id: "inherit-model".into(), name: "spawn_agent".into(), arguments: json!({"task_name":"inherited", "task":"Inspect the source and preserve the findings"}).to_string(),
    }).await.unwrap()).unwrap();
    let child_id = spawned["agent_id"].as_str().unwrap();
    wait_for_child_turn(&fixture.state, child_id).await;
    let history_before = fixture.state.get(child_id).unwrap();
    assert_eq!(history_before.summary.model_id, original.id);
    assert_eq!(
        history_before.summary.reasoning_effort.as_deref(),
        Some("low")
    );
    assert!(
        JsonlEventStore::new(fixture.directory.join("events"))
            .load_for_runtime(child_id)
            .await
            .unwrap()
            .unwrap()
            .events
            .iter()
            .any(|event| matches!(
                &event.event,
                RunEvent::ModelResponseReceived {
                    provider_state: Some(_),
                    ..
                }
            ))
    );

    fixture
        .state
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::WaitingForInput)
        .unwrap();
    fixture
        .change("parent", &replacement.id, Some("high"))
        .await
        .unwrap();
    fixture
        .state
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::Running)
        .unwrap();
    fixture.state.collaboration_call("parent", agent_runtime::ToolCall {
        id:"resume-new-model".into(), name:"agent_control".into(), arguments:json!({"action":"resume","agent_id":child_id,"message":"Continue checking the previous result"}).to_string(),
    }).await.unwrap();
    wait_for_child_turn(&fixture.state, child_id).await;
    let after = fixture.state.get(child_id).unwrap();
    assert_eq!(after.summary.model_id, replacement.id);
    assert_eq!(after.summary.reasoning_effort.as_deref(), Some("high"));
    assert!(after.summary.model_history_through.is_some());
    assert!(after.messages.iter().any(|message| {
        message
            .text
            .contains("Verified result from the previous child turn")
    }));
    {
        let requests = target_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "target-model");
        assert_eq!(requests[0]["reasoning"]["effort"], "high");
        let body = requests[0].to_string();
        assert!(body.contains("Inspect the source and preserve the findings"));
        assert!(body.contains("Verified result from the previous child turn"));
        assert!(!body.contains("private-source-reasoning-marker"));
        assert!(!body.contains("reasoning_content"));
    }

    // An explicit per-task choice stays pinned instead of being overwritten by
    // the coordinator's current selection when the task is continued.
    let pinned: Value = serde_json::from_str(&fixture.state.collaboration_call("parent", agent_runtime::ToolCall {
        id:"pinned-model".into(), name:"spawn_agent".into(), arguments:json!({"task_name":"pinned","task":"Check the evidence independently","model":{"model_id":original.id,"reasoning_effort":"low"}}).to_string(),
    }).await.unwrap()).unwrap();
    let pinned_id = pinned["agent_id"].as_str().unwrap();
    wait_for_child_turn(&fixture.state, pinned_id).await;
    fixture.state.collaboration_call("parent", agent_runtime::ToolCall {
        id:"resume-pinned-model".into(), name:"agent_control".into(), arguments:json!({"action":"resume","agent_id":pinned_id,"message":"Recheck the previous evidence"}).to_string(),
    }).await.unwrap();
    wait_for_child_turn(&fixture.state, pinned_id).await;
    assert_eq!(
        fixture.state.get(pinned_id).unwrap().summary.model_id,
        original.id
    );
    assert_eq!(source_requests.lock().unwrap().len(), 3);
    assert_eq!(target_requests.lock().unwrap().len(), 1);
    fixture.state.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn cancellation_waiting_on_old_binding_stops_the_rebound_execution() {
    let fixture = Fixture::new().await;
    let entry = fixture.conversation("selected").await;
    let request_started = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let notification = request_started.clone();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let notification = notification.clone();
                    async move {
                        notification.notify_one();
                        std::future::pending::<Json<Value>>().await
                    }
                }),
            ),
        )
        .into_future(),
    );
    let target = fixture
        .state
        .configure_model(config(
            ProviderProtocol::ChatCompletions,
            &url,
            "slow-target",
        ))
        .await
        .unwrap();
    let gate = fixture.state.inner.model_gate.lock().await;
    let operation = entry.operations.lock().await;
    let cancelling = fixture.state.clone();
    // Background parent cleanup does not own model_gate. It can already be
    // waiting on this run's operation lock while a resume changes its model.
    let cancellation =
        tokio::spawn(async move { cancelling.cancel_member("selected", false).await });
    tokio::task::yield_now().await;
    assert!(!cancellation.is_finished());
    let rebound = fixture
        .state
        .rebind_idle_model(
            &entry,
            UpdateRunModelInput {
                model_id: target.id,
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&entry.operations, &rebound.operations));
    let mut run = rebound
        .runtime
        .restore_run("selected")
        .await
        .unwrap()
        .unwrap();
    rebound
        .runtime
        .submit_input(&mut run, "Continue with the new model")
        .await
        .unwrap();
    execute(fixture.state.clone(), rebound.clone(), run);
    tokio::time::timeout(Duration::from_secs(5), request_started.notified())
        .await
        .unwrap();
    drop(operation);
    drop(gate);
    let cancelled = tokio::time::timeout(Duration::from_secs(5), cancellation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.summary.status, RunStatus::Cancelled);
    tokio::time::timeout(Duration::from_secs(5), async {
        while rebound.background.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!rebound.runtime.is_executing("selected"));
    fixture.state.shutdown().await;
    server.abort();
}
