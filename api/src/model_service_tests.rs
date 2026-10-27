use super::*;
use agent_core::agent::RunEvent;
use agent_runtime::{
    ContextCheckpoint, ContextCheckpointStore, ContextMeasurement, ContextSummary, EventStore,
    ModelResponse,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    directory: PathBuf,
    state: AppState,
    old: ModelInfo,
    replacement: ModelInfo,
}

fn input(window: usize) -> ConfigureModel {
    ConfigureModel {
        protocol: agent_providers::ProviderProtocol::ChatCompletions,
        // No model service is running here: updating capacity must be entirely local.
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key: "fixture-key".into(),
        model: "fixture-model".into(),
        context_window: window,
        max_output_tokens: 2048,
        reasoning_mode: "off".into(),
        reasoning_effort: "medium".into(),
        reasoning_capability: None,
        reasoning_default: None,
        supports_images: false,
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::with_base_url("http://127.0.0.1:1/v1").await
    }

    async fn with_base_url(base_url: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-model-replacement-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let mut original = input(8192);
        original.base_url = base_url.into();
        let mut updated = input(1_000_000);
        updated.base_url = base_url.into();
        let old = state.configure_model(original).await.unwrap();
        let replacement = state.configure_model(updated).await.unwrap();
        Self {
            directory,
            state,
            old,
            replacement,
        }
    }

    async fn conversation(&self, id: &str) -> Arc<RunEntry> {
        let detail = RunDetail {
            summary: RunSummary {
                reasoning_settings_version: 1,
                reasoning_effort: None,
                model_history_through: None,
                delegation: None,
                execution_policy: Default::default(),
                id: id.into(),
                revision: 0,
                title: "Capacity fixture".into(),
                prompt: "Remember the existing history".into(),
                status: RunStatus::Running,
                compacting: false,
                model_id: self.old.id.clone(),
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
        };
        let entry = self.state.make_entry(detail, 0).unwrap();
        let mut run = entry
            .runtime
            .start_run(&entry.agent, id, "Remember the existing history")
            .await
            .unwrap();
        run.begin_step().unwrap();
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::ModelResponseReceived {
                    content: "Remembered".into(),
                    response: Some(ModelResponse::final_answer("Remembered".into())),
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
                    prompt: "Remembered".into(),
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

    async fn replace(&self) -> Result<ModelInfo, ServiceError> {
        self.state
            .replace_model(&self.old.id, &self.replacement.id)
            .await
    }
}

async fn wait_for_completed_child(state: &AppState, id: &str) {
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
async fn replacing_capacity_migrates_pinned_child_models_and_preserves_followup_after_restart() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(move |Json(body): Json<Value>| {
                    recorded.lock().unwrap().push(body);
                    async {
                        Json(json!({
                            "id":"capacity-child-response",
                            "choices":[{"message":{"role":"assistant","content":"Verified child result"},"finish_reason":"stop"}],
                            "usage":{"prompt_tokens":100,"completion_tokens":10}
                        }))
                    }
                }),
            ),
        )
        .into_future(),
    );
    let fixture = Fixture::with_base_url(&base_url).await;
    let mut other_config = input(8192);
    other_config.base_url = base_url.clone();
    other_config.model = "other-model".into();
    let other = fixture.state.configure_model(other_config).await.unwrap();
    let parent = fixture.conversation("parent").await;
    parent
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::Running)
        .unwrap();
    let mut children = Vec::new();
    for (task, model_id, effort) in [
        ("migrated", fixture.old.id.as_str(), "low"),
        ("unrelated", other.id.as_str(), "high"),
    ] {
        let spawned: Value = serde_json::from_str(
            &fixture
                .state
                .collaboration_call(
                    "parent",
                    agent_runtime::ToolCall {
                        id: format!("spawn-{task}"),
                        name: "spawn_agent".into(),
                        arguments: json!({
                            "task_name":task,"task":"Check the evidence",
                            "model":{"model_id":model_id,"reasoning_effort":effort}
                        })
                        .to_string(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let id = spawned["agent_id"].as_str().unwrap().to_owned();
        wait_for_completed_child(&fixture.state, &id).await;
        children.push(id);
    }
    parent
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::WaitingForInput)
        .unwrap();
    let unrelated_before = serde_json::to_value(fixture.state.get(&children[1]).unwrap()).unwrap();
    fixture.replace().await.unwrap();

    let migrated = fixture.state.get(&children[0]).unwrap().summary;
    assert_eq!(migrated.model_id, fixture.replacement.id);
    let selection = migrated.delegation.unwrap().model_override.unwrap();
    assert_eq!(selection.model_id, fixture.replacement.id);
    assert_eq!(selection.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(migrated.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(
        serde_json::to_value(fixture.state.get(&children[1]).unwrap()).unwrap(),
        unrelated_before
    );
    fixture.state.shutdown().await;

    // Restore only the replacement and the unrelated connection. A stale pin
    // cannot silently fall back to an old profile still registered in memory.
    let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    let mut replacement_config = input(1_000_000);
    replacement_config.base_url = base_url.clone();
    restored.configure_model(replacement_config).await.unwrap();
    let mut other_config = input(8192);
    other_config.base_url = base_url;
    other_config.model = "other-model".into();
    restored.configure_model(other_config).await.unwrap();
    restored
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::Running)
        .unwrap();
    for (id, expected_model, effort) in [
        (&children[0], &fixture.replacement.id, "low"),
        (&children[1], &other.id, "high"),
    ] {
        let saved = restored.get(id).unwrap().summary;
        assert_eq!(&saved.model_id, expected_model);
        assert_eq!(
            saved.delegation.unwrap().model_override.unwrap(),
            crate::contracts::UpdateRunModelInput {
                model_id: expected_model.clone(),
                reasoning_effort: Some(effort.into()),
            }
        );
        restored
            .collaboration_call(
                "parent",
                agent_runtime::ToolCall {
                    id: format!("followup-{id}"),
                    name: "agent_control".into(),
                    arguments: json!({
                        "action":"followup","target":id,"message":"Recheck the evidence"
                    })
                    .to_string(),
                },
            )
            .await
            .unwrap();
        wait_for_completed_child(&restored, id).await;
        assert_eq!(&restored.get(id).unwrap().summary.model_id, expected_model);
    }
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2]["model"], "fixture-model");
        assert_eq!(requests[2]["reasoning_effort"], "low");
        assert_eq!(requests[3]["model"], "other-model");
        assert_eq!(requests[3]["reasoning_effort"], "high");
    }
    restored.shutdown().await;
    server.abort();
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn replacing_capacity_updates_idle_runs_and_subscriptions_without_rewriting_history() {
    let fixture = Fixture::new().await;
    let entry = fixture.conversation("existing-run").await;
    let checkpoint_path = fixture.directory.join("checkpoints/existing-run.json");
    let store =
        crate::context::DiskCheckpoints::new(checkpoint_path.clone(), entry.snapshot.clone())
            .unwrap();
    store
        .save(ContextCheckpoint {
            run_id: "existing-run".into(),
            id: "existing-checkpoint".into(),
            through_sequence: 0,
            source_fingerprint: "fixture-prefix".into(),
            before_tokens: 2000,
            after_tokens: 500,
            summary: ContextSummary {
                objective: "Remember the existing history".into(),
                ..Default::default()
            },
            pruning: None,
        })
        .await
        .unwrap();
    entry
        .snapshot
        .update(|detail| {
            let context = detail.context.as_mut().unwrap();
            let mut accounting = ContextMeasurement::estimated(1234);
            accounting.source = agent_runtime::ContextMeasurementSource::ProviderCalibrated;
            accounting.measured_input_tokens = Some(1300);
            context.accounting = Some(accounting);
            context.measurement = "provider_calibrated".into();
        })
        .unwrap();
    let before = fixture.state.get("existing-run").unwrap();
    let checkpoints = fs::read(&checkpoint_path).unwrap();
    let events = JsonlEventStore::new(fixture.directory.join("events"));
    let history = serde_json::to_value(
        events
            .load_for_runtime("existing-run")
            .await
            .unwrap()
            .unwrap()
            .events,
    )
    .unwrap();
    let mut subscription = fixture.state.subscribe("existing-run").unwrap();
    fixture.replace().await.unwrap();

    assert!(subscription.has_changed().unwrap());
    let after = subscription.borrow_and_update().clone();
    assert_eq!(after.summary.model_id, fixture.replacement.id);
    assert!(after.summary.revision > before.summary.revision);
    assert_eq!(after.summary.status, before.summary.status);
    assert_eq!(
        serde_json::to_value(&after.messages).unwrap(),
        serde_json::to_value(&before.messages).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&after.events).unwrap(),
        serde_json::to_value(&before.events).unwrap()
    );
    assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoints);
    assert_eq!(
        serde_json::to_value(
            events
                .load_for_runtime("existing-run")
                .await
                .unwrap()
                .unwrap()
                .events
        )
        .unwrap(),
        history
    );
    let context = after.context.unwrap();
    assert_eq!(context.window_tokens, 1_000_000);
    assert_eq!(context.input_budget, 950_000);
    assert_eq!(context.trigger_tokens, 900_000);
    assert_eq!(context.target_tokens, 675_000);
    assert_eq!(context.reserved_output_tokens, 2048);
    assert_eq!(context.compaction_count, 1);
    assert_eq!(context.estimated_input_tokens, 1234);
    assert_eq!(context.measurement, "estimated");
    assert!(context.accounting.is_none());
    assert!(
        !fixture
            .state
            .config()
            .models
            .iter()
            .any(|model| model.id == fixture.old.id)
    );
    let updated = fixture.state.entry("existing-run").unwrap();
    assert_eq!(
        updated
            .runtime
            .model()
            .context_metadata()
            .unwrap()
            .context_window,
        1_000_000
    );
    assert_eq!(
        updated.runtime.model().provider_context_identity(),
        entry.runtime.model().provider_context_identity()
    );
    let revision = fixture.state.get("existing-run").unwrap().summary.revision;
    // Simulate a response lost after the successful replacement.
    assert_eq!(fixture.replace().await.unwrap().id, fixture.replacement.id);
    assert_eq!(
        fixture.state.get("existing-run").unwrap().summary.revision,
        revision
    );

    fixture.state.shutdown().await;
    let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    restored.configure_model(input(1_000_000)).await.unwrap();
    let detail = restored.get("existing-run").unwrap();
    assert_eq!(detail.summary.model_id, fixture.replacement.id);
    assert_eq!(detail.context.unwrap().window_tokens, 1_000_000);
    assert!(
        restored
            .entry("existing-run")
            .unwrap()
            .runtime
            .model()
            .available()
    );
    assert_eq!(
        serde_json::to_value(detail.messages).unwrap(),
        serde_json::to_value(before.messages).unwrap()
    );
    restored.shutdown().await;
}

#[tokio::test]
async fn busy_runs_prevent_any_model_replacement() {
    for blocker in ["running", "background", "compacting"] {
        let fixture = Fixture::new().await;
        fixture.conversation("first-idle").await;
        let active = fixture.conversation("second-busy").await;
        match blocker {
            "running" => active
                .snapshot
                .update(|detail| detail.summary.status = RunStatus::Running)
                .unwrap(),
            "compacting" => active
                .snapshot
                .update(|detail| detail.summary.compacting = true)
                .unwrap(),
            _ => active.background.store(true, Ordering::Release),
        }
        assert!(
            matches!(fixture.replace().await, Err(ServiceError::Conflict(_))),
            "{blocker}"
        );
        for id in ["first-idle", "second-busy"] {
            assert_eq!(
                fixture.state.get(id).unwrap().summary.model_id,
                fixture.old.id
            );
            assert_eq!(
                fixture
                    .state
                    .get(id)
                    .unwrap()
                    .context
                    .unwrap()
                    .window_tokens,
                8192
            );
        }
        assert!(
            fixture
                .state
                .config()
                .models
                .iter()
                .any(|model| model.id == fixture.old.id)
        );
        active.background.store(false, Ordering::Release);
    }
}

#[tokio::test]
async fn replacement_rejects_provider_changes_and_supports_identity_noops() {
    let fixture = Fixture::new().await;
    fixture.conversation("original").await;
    for change in ["provider", "model", "protocol", "reasoning"] {
        let mut config = input(1_000_000);
        match change {
            "provider" => config.base_url = "http://127.0.0.1:2/v1".into(),
            "model" => config.model = "another-model".into(),
            "protocol" => config.protocol = agent_providers::ProviderProtocol::Responses,
            _ => config.reasoning_mode = "standard".into(),
        }
        let other = fixture.state.configure_model(config).await.unwrap();
        assert!(
            matches!(
                fixture
                    .state
                    .replace_model(&fixture.old.id, &other.id)
                    .await,
                Err(ServiceError::BadRequest(_))
            ),
            "{change}"
        );
    }
    assert_eq!(
        fixture.state.get("original").unwrap().summary.model_id,
        fixture.old.id
    );
    let before = fixture.state.get("original").unwrap().summary.revision;
    assert_eq!(
        fixture
            .state
            .replace_model(&fixture.old.id, &fixture.old.id)
            .await
            .unwrap()
            .id,
        fixture.old.id
    );
    assert_eq!(
        fixture.state.get("original").unwrap().summary.revision,
        before
    );
    assert!(matches!(
        fixture
            .state
            .replace_model(MODEL_ID, &fixture.replacement.id)
            .await,
        Err(ServiceError::BadRequest(_))
    ));
    assert!(matches!(
        fixture
            .state
            .replace_model(&fixture.old.id, "missing-model")
            .await,
        Err(ServiceError::NotFound)
    ));
}

#[tokio::test]
async fn missing_source_is_idempotent_only_when_no_conversations_still_reference_it() {
    let fixture = Fixture::new().await;
    fixture.conversation("orphaned").await;
    fixture
        .state
        .inner
        .models
        .write()
        .unwrap()
        .remove(&fixture.old.id);
    assert!(matches!(
        fixture.replace().await,
        Err(ServiceError::NotFound)
    ));
    assert_eq!(
        fixture.state.get("orphaned").unwrap().summary.model_id,
        fixture.old.id
    );
    assert!(matches!(
        fixture
            .state
            .replace_model("missing", "missing-target")
            .await,
        Err(ServiceError::NotFound)
    ));
    assert_eq!(
        fixture
            .state
            .replace_model("missing", &fixture.replacement.id)
            .await
            .unwrap()
            .id,
        fixture.replacement.id
    );
}

#[tokio::test]
async fn partial_persistence_failure_keeps_both_model_configurations_and_can_be_retried() {
    let fixture = Fixture::new().await;
    fixture.conversation("first-run").await;
    let second = fixture.conversation("second-run").await;
    let before = serde_json::to_value(second.snapshot.get()).unwrap();
    let temporary = fixture.directory.join("runs/second-run.json.tmp");
    fs::create_dir(&temporary).unwrap();
    assert!(matches!(
        fixture.replace().await,
        Err(ServiceError::Internal(_))
    ));
    assert_eq!(
        fixture.state.get("first-run").unwrap().summary.model_id,
        fixture.replacement.id
    );
    assert_eq!(serde_json::to_value(second.snapshot.get()).unwrap(), before);
    assert_eq!(
        fixture
            .state
            .entry("second-run")
            .unwrap()
            .runtime
            .model()
            .context_metadata()
            .unwrap()
            .context_window,
        8192
    );
    let models = fixture.state.config().models;
    assert!(models.iter().any(|model| model.id == fixture.old.id));
    assert!(
        models
            .iter()
            .any(|model| model.id == fixture.replacement.id)
    );
    fs::remove_dir(temporary).unwrap();
    fixture.replace().await.unwrap();
    assert_eq!(
        fixture.state.get("second-run").unwrap().summary.model_id,
        fixture.replacement.id
    );
}

#[tokio::test]
async fn replacement_route_uses_only_registered_models_and_returns_public_metadata() {
    let fixture = Fixture::new().await;
    let response = crate::app(fixture.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/models/{}/replace", fixture.old.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"replacement_model_id": fixture.replacement.id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 100_000).await.unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["id"], fixture.replacement.id);
    assert_eq!(result["context_window"], 1_000_000);
    assert!(!String::from_utf8_lossy(&bytes).contains("fixture-key"));
}
