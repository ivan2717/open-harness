use super::*;
use agent_providers::ProviderProtocol;
#[cfg(any(target_os = "macos", windows))]
use agent_runtime::ToolExecutor;
use axum::http::HeaderMap;

fn edited_input(protocol: ProviderProtocol, url: &str) -> ConfigureModel {
    let mut config = input(1_000_000);
    config.protocol = protocol;
    config.base_url = url.into();
    config.api_key = "edited-fixture-key".into();
    config.model = "edited-model".into();
    config.supports_images = true;
    config.max_output_tokens = 16_384;
    config.reasoning_capability = Some(serde_json::from_value(json!({
        "source":"manual",
        "adapter": if protocol == ProviderProtocol::AnthropicMessages {"anthropic_budget"} else {"effort"},
        "options": if protocol == ProviderProtocol::AnthropicMessages {
            json!([{"id":"low","budget_tokens":1024}])
        } else {
            json!([{"id":"low","effort":"low"}])
        }
    })).unwrap());
    config.reasoning_default = Some("low".into());
    config
}

async fn wait_for_input(state: &AppState, id: &str) {
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

async fn configure_via_route(state: &AppState, body: Value) -> StatusCode {
    crate::app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn edit_registration_rejects_unrelated_profile_collisions_and_busy_sources() {
    let fixture = Fixture::new().await;
    let source = fixture.conversation("source").await;
    let mut body = json!({
        "protocol":"chat_completions", "base_url":"http://127.0.0.1:1/v1",
        "model":"fixture-model", "api_key":"do-not-overwrite-another-profile",
        "context_window":1_000_000, "max_output_tokens":2048,
        "reasoning_mode":"off", "reasoning_effort":"medium", "supports_images":true,
        "edit_source_id":fixture.old.id,
    });
    assert_eq!(
        configure_via_route(&fixture.state, body.clone()).await,
        StatusCode::CONFLICT
    );
    assert!(
        !fixture
            .state
            .config()
            .models
            .iter()
            .find(|m| m.id == fixture.replacement.id)
            .unwrap()
            .supports_images
    );
    assert_eq!(
        fixture.state.get("source").unwrap().summary.model_id,
        fixture.old.id
    );

    // A new target ID still cannot be staged while its source has live work.
    body["model"] = json!("new-edited-model");
    source.background.store(true, Ordering::Release);
    let count = fixture.state.config().models.len();
    assert_eq!(
        configure_via_route(&fixture.state, body.clone()).await,
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.state.config().models.len(), count);
    source.background.store(false, Ordering::Release);
    body["edit_source_id"] = json!("missing-profile");
    assert_eq!(
        configure_via_route(&fixture.state, body.clone()).await,
        StatusCode::NOT_FOUND
    );
    body["edit_source_id"] = json!(fixture.old.id);
    body["unexpected_option"] = json!(true);
    assert_eq!(
        configure_via_route(&fixture.state, body.clone()).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    body.as_object_mut().unwrap().remove("unexpected_option");
    assert_eq!(
        configure_via_route(&fixture.state, body).await,
        StatusCode::OK
    );
    // Registration stages a new profile; migration is a separate durable step.
    assert_eq!(
        fixture.state.get("source").unwrap().summary.model_id,
        fixture.old.id
    );
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn explicit_profile_edit_preserves_history_and_uses_the_new_protocol_after_restart() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        let fixture = Fixture::new().await;
        fixture.conversation("edited").await;
        fixture
            .state
            .update_run_model(
                "edited",
                crate::contracts::UpdateRunModelInput {
                    model_id: fixture.old.id.clone(),
                    reasoning_effort: Some("high".into()),
                },
            )
            .await
            .unwrap();
        let (endpoint, response) = match protocol {
            ProviderProtocol::ChatCompletions => (
                "chat/completions",
                json!({"id":"chat-result","choices":[{"message":{"role":"assistant","content":"Continued"},"finish_reason":"stop"}]}),
            ),
            ProviderProtocol::Responses => (
                "responses",
                json!({"id":"responses-result","status":"completed","output":[{"id":"m1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Continued"}]}]}),
            ),
            ProviderProtocol::AnthropicMessages => (
                "messages",
                json!({"id":"messages-result","type":"message","role":"assistant","content":[{"type":"text","text":"Continued"}],"stop_reason":"end_turn"}),
            ),
        };
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new().route(
                    &format!("/v1/{endpoint}"),
                    post(move |headers: HeaderMap, Json(body): Json<Value>| {
                        recorded.lock().unwrap().push((headers, body));
                        let response = response.clone();
                        async { Json(response) }
                    }),
                ),
            )
            .into_future(),
        );
        let target = fixture
            .state
            .configure_model(edited_input(protocol, &url))
            .await
            .unwrap();
        let before = fixture.state.get("edited").unwrap();
        let store = JsonlEventStore::new(fixture.directory.join("events"));
        let history = serde_json::to_value(
            store
                .load_for_runtime("edited")
                .await
                .unwrap()
                .unwrap()
                .events,
        )
        .unwrap();
        let mut subscription = fixture.state.subscribe("edited").unwrap();
        // Old callers cannot accidentally make a provider-changing migration.
        assert!(matches!(
            fixture
                .state
                .replace_model(&fixture.old.id, &target.id)
                .await,
            Err(ServiceError::BadRequest(_))
        ));
        let response = crate::app(fixture.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/models/{}/replace", fixture.old.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"replacement_model_id":target.id,"allow_configuration_change":true})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(subscription.has_changed().unwrap());
        let after = subscription.borrow_and_update().clone();
        assert_eq!(after.summary.model_id, target.id);
        assert_eq!(
            after.summary.reasoning_effort, None,
            "unsupported high must use new default"
        );
        assert_eq!(after.summary.reasoning_settings_version, 1);
        assert!(after.summary.model_history_through.is_some());
        assert_eq!(after.summary.status, before.summary.status);
        assert_eq!(
            serde_json::to_value(after.messages).unwrap(),
            serde_json::to_value(before.messages).unwrap()
        );
        assert_eq!(
            serde_json::to_value(
                store
                    .load_for_runtime("edited")
                    .await
                    .unwrap()
                    .unwrap()
                    .events,
            )
            .unwrap(),
            history
        );
        assert_eq!(after.context.unwrap().window_tokens, 1_000_000);
        assert!(
            fixture
                .state
                .entry("edited")
                .unwrap()
                .runtime
                .model()
                .capabilities()
                .supports_images
        );
        let boundary = after.summary.model_history_through;
        fixture.state.shutdown().await;
        let restored = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        restored
            .configure_model(edited_input(protocol, &url))
            .await
            .unwrap();
        assert_eq!(
            restored
                .get("edited")
                .unwrap()
                .summary
                .model_history_through,
            boundary
        );
        restored
            .input("edited", "Continue the previous task".into())
            .await
            .unwrap();
        wait_for_input(&restored, "edited").await;
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let (headers, body) = &requests[0];
            assert_eq!(body["model"], "edited-model");
            assert!(body.to_string().contains("Remember the existing history"));
            assert!(body.to_string().contains("Remembered"));
            match protocol {
                ProviderProtocol::ChatCompletions => assert_eq!(body["reasoning_effort"], "low"),
                ProviderProtocol::Responses => assert_eq!(body["reasoning"]["effort"], "low"),
                ProviderProtocol::AnthropicMessages => {
                    assert_eq!(body["thinking"]["budget_tokens"], 1024)
                }
            }
            let credential = if protocol == ProviderProtocol::AnthropicMessages {
                "x-api-key"
            } else {
                "authorization"
            };
            assert!(
                headers[credential]
                    .to_str()
                    .unwrap()
                    .ends_with("edited-fixture-key")
            );
        }
        restored.shutdown().await;
        server.abort();
    }
}

#[tokio::test]
async fn vision_and_key_edits_keep_profile_identity_and_update_idle_runs_only() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        recorded.lock().unwrap().push((headers, body));
        async { Json(json!({"id":"vision-response","choices":[{"message":{"role":"assistant","content":"Saw the screenshot"},"finish_reason":"stop"}]})) }
    }))).into_future());
    let fixture = Fixture::with_base_url(&url, 128_000).await;
    let entry = fixture.conversation("vision").await;
    let changed = || {
        let mut config = input(128_000);
        config.base_url = url.clone();
        config.api_key = "rotated-fixture-key".into();
        config.supports_images = true;
        config
    };
    entry.background.store(true, Ordering::Release);
    assert!(matches!(
        fixture.state.configure_model(changed()).await,
        Err(ServiceError::Conflict(_))
    ));
    assert!(
        !fixture
            .state
            .config()
            .models
            .iter()
            .find(|m| m.id == fixture.old.id)
            .unwrap()
            .supports_images
    );
    entry.background.store(false, Ordering::Release);
    let updated = fixture.state.configure_model(changed()).await.unwrap();
    assert_eq!(updated.id, fixture.old.id);
    assert!(updated.supports_images);
    assert!(
        fixture
            .state
            .entry("vision")
            .unwrap()
            .runtime
            .model()
            .capabilities()
            .supports_images
    );
    assert_eq!(
        fixture
            .state
            .get("vision")
            .unwrap()
            .summary
            .model_history_through,
        None
    );
    let image = serde_json::from_value(json!({"name":"screenshot.png","image":{"mime_type":"image/png","data_base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZcZkAAAAASUVORK5CYII="}})).unwrap();
    fixture
        .state
        .input_with_files("vision", "Describe this".into(), vec![image])
        .await
        .unwrap();
    wait_for_input(&fixture.state, "vision").await;
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0["authorization"], "Bearer rotated-fixture-key");
        assert!(
            requests[0].1["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |message| message["content"].as_array().is_some_and(|content| content
                        .iter()
                        .any(|block| block["type"] == "image_url"))
                )
        );
    }
    fixture.state.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn profile_edits_migrate_child_pins_and_reject_busy_future_references() {
    let fixture = Fixture::new().await;
    fixture.conversation("parent").await;
    let child = fixture.conversation("child").await;
    let pin_only = fixture.conversation("pin-only").await;
    for entry in [&child, &pin_only] {
        entry
            .snapshot
            .update(|detail| {
                detail.summary.reasoning_effort = Some("high".into());
                detail.summary.delegation = Some(crate::contracts::Delegation {
                    parent_run_id: "parent".into(),
                    root_run_id: "parent".into(),
                    task_name: detail.summary.id.clone(),
                    write_paths: None,
                    model_override: Some(crate::contracts::UpdateRunModelInput {
                        model_id: fixture.old.id.clone(),
                        reasoning_effort: Some("high".into()),
                    }),
                    spawn_call_id: "fixture-spawn".into(),
                });
            })
            .unwrap();
    }
    let unrelated = fixture
        .state
        .rebind_idle_model(
            &pin_only,
            crate::contracts::UpdateRunModelInput {
                model_id: fixture.replacement.id.clone(),
                reasoning_effort: Some("low".into()),
            },
        )
        .await
        .unwrap();
    let target = fixture
        .state
        .configure_model(edited_input(
            ProviderProtocol::Responses,
            "http://127.0.0.1:2/v1",
        ))
        .await
        .unwrap();
    for blocker in ["running", "background", "compacting"] {
        unrelated
            .snapshot
            .update(|detail| {
                detail.summary.status = if blocker == "running" {
                    RunStatus::Running
                } else {
                    RunStatus::WaitingForInput
                };
                detail.summary.compacting = blocker == "compacting";
            })
            .unwrap();
        unrelated
            .background
            .store(blocker == "background", Ordering::Release);
        assert!(
            matches!(
                fixture
                    .state
                    .replace_model_configuration(&fixture.old.id, &target.id, true)
                    .await,
                Err(ServiceError::Conflict(_))
            ),
            "{blocker}"
        );
        assert_eq!(
            fixture.state.get("parent").unwrap().summary.model_id,
            fixture.old.id
        );
        assert_eq!(
            fixture.state.get("child").unwrap().summary.model_id,
            fixture.old.id
        );
    }
    unrelated
        .snapshot
        .update(|detail| detail.summary.compacting = false)
        .unwrap();
    unrelated.background.store(false, Ordering::Release);
    let unrelated_context = serde_json::to_value(unrelated.snapshot.get().context).unwrap();
    fixture
        .state
        .replace_model_configuration(&fixture.old.id, &target.id, true)
        .await
        .unwrap();
    let child = fixture.state.get("child").unwrap().summary;
    assert_eq!(child.model_id, target.id);
    assert_eq!(child.reasoning_effort, None);
    for id in ["child", "pin-only"] {
        let selection = fixture
            .state
            .get(id)
            .unwrap()
            .summary
            .delegation
            .unwrap()
            .model_override
            .unwrap();
        assert_eq!(selection.model_id, target.id);
        assert_eq!(selection.reasoning_effort, None);
    }
    let pin_only = fixture.state.get("pin-only").unwrap();
    assert_eq!(pin_only.summary.model_id, fixture.replacement.id);
    assert_eq!(pin_only.summary.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(
        serde_json::to_value(pin_only.context).unwrap(),
        unrelated_context
    );
    assert!(Arc::ptr_eq(
        &fixture.state.entry("pin-only").unwrap(),
        &unrelated
    ));
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn compatible_effort_survives_and_partial_edits_are_safely_retryable() {
    let fixture = Fixture::new().await;
    let first = fixture.conversation("first").await;
    let second = fixture.conversation("second").await;
    first
        .snapshot
        .update(|detail| detail.summary.reasoning_effort = Some("low".into()))
        .unwrap();
    let target = fixture
        .state
        .configure_model(edited_input(
            ProviderProtocol::Responses,
            "http://127.0.0.1:2/v1",
        ))
        .await
        .unwrap();
    let before = serde_json::to_value(second.snapshot.get()).unwrap();
    let temporary = fixture.directory.join("runs/second.json.tmp");
    fs::create_dir(&temporary).unwrap();
    assert!(matches!(
        fixture
            .state
            .replace_model_configuration(&fixture.old.id, &target.id, true)
            .await,
        Err(ServiceError::Internal(_))
    ));
    assert_eq!(serde_json::to_value(second.snapshot.get()).unwrap(), before);
    let changed = fixture.state.get("first").unwrap().summary;
    assert_eq!(changed.model_id, target.id);
    assert_eq!(changed.reasoning_effort.as_deref(), Some("low"));
    assert!(changed.model_history_through.is_some());
    assert!(
        fixture
            .state
            .config()
            .models
            .iter()
            .any(|m| m.id == fixture.old.id)
    );
    fs::remove_dir(temporary).unwrap();
    fixture
        .state
        .replace_model_configuration(&fixture.old.id, &target.id, true)
        .await
        .unwrap();
    let after = fixture.state.get("second").unwrap().summary;
    assert_eq!(after.model_id, target.id);
    assert!(after.model_history_through.is_some());
    let revision = after.revision;
    fixture
        .state
        .replace_model_configuration(&fixture.old.id, &target.id, true)
        .await
        .unwrap();
    assert_eq!(
        fixture.state.get("second").unwrap().summary.revision,
        revision
    );
    fixture.state.shutdown().await;
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn pending_commands_block_profile_edits_before_any_binding_changes() {
    let fixture = Fixture::new().await;
    fixture.conversation("first-idle").await;
    let entry = fixture.conversation("command-owner").await;
    let tools = entry.workspace.as_ref().unwrap().clone();
    let worker = tools.clone();
    let command = tokio::spawn(async move {
        worker
            .execute(agent_runtime::ToolCall {
                id: "pending-command".into(),
                name: "run_command".into(),
                arguments: json!({"command":"echo profile-test","access":"read_only","network_access":"allowed"}).to_string(),
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while tools.review().unwrap().approvals.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(tools.commands_running());
    let target = fixture
        .state
        .configure_model(edited_input(
            ProviderProtocol::Responses,
            "http://127.0.0.1:2/v1",
        ))
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .state
            .replace_model_configuration(&fixture.old.id, &target.id, true)
            .await,
        Err(ServiceError::Conflict(_))
    ));
    assert_eq!(
        fixture.state.get("first-idle").unwrap().summary.model_id,
        fixture.old.id
    );
    tools.cancel_commands_and_wait().await.unwrap();
    assert!(command.await.unwrap().is_err());
    fixture.state.shutdown().await;
}
