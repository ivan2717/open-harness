use super::*;
use crate::contracts::{Delegation, ExecutionPolicy};
use agent_core::agent::RunEvent;
#[cfg(any(target_os = "macos", windows))]
use agent_local::{CommandAccess, CommandGrant, LocalTools, NetworkAccess};
use agent_runtime::{ModelResponse, ToolCall, ToolExecutor};
use serde_json::json;

struct Fixture {
    directory: PathBuf,
    project: PathBuf,
    state: AppState,
}
impl Fixture {
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("openharness-permissions-{}", new_run_id()));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        let project = fs::canonicalize(project).unwrap();
        let state = AppState::open_with_delay(directory.join("data"), None, Duration::ZERO)
            .await
            .unwrap();
        Self {
            directory,
            project,
            state,
        }
    }

    async fn conversation(
        &self,
        id: &str,
        parent: Option<Option<Vec<String>>>,
        model: Option<&str>,
    ) -> Arc<RunEntry> {
        let detail = RunDetail {
            summary: RunSummary {
                delegation: parent.map(|write_paths| Delegation {
                    parent_run_id: "parent".into(),
                    root_run_id: "parent".into(),
                    task_name: id.into(),
                    write_paths,
                    model_override: None,
                    spawn_call_id: format!("spawn-{id}"),
                }),
                execution_policy: ExecutionPolicy::default(),
                id: id.into(),
                revision: 0,
                title: id.into(),
                prompt: "Inspect the public documentation without modifying project files".into(),
                status: RunStatus::Running,
                compacting: false,
                model_id: model.unwrap_or("permissions-fixture-unavailable").into(),
                reasoning_effort: None,
                reasoning_settings_version: 1,
                model_history_through: None,
                workspace_path: Some(self.project.display().to_string()),
                execution_directory: None,
                permission_mode: PermissionMode::WorkspaceWrite,
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
            .start_run(&entry.agent, id, "Inspect the public documentation")
            .await
            .unwrap();
        run.begin_step().unwrap();
        entry
            .runtime
            .record_event(
                &mut run,
                RunEvent::ModelResponseReceived {
                    content: "Ready".into(),
                    response: Some(ModelResponse::input_required("Ready".into())),
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
                    prompt: "Ready".into(),
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

    async fn close(self) {
        self.state.shutdown().await;
        fs::remove_dir_all(self.directory).unwrap();
    }
}

#[tokio::test]
async fn mode_changes_apply_to_idle_family_without_widening_narrowed_children() {
    let fixture = Fixture::new().await;
    fixture.conversation("parent", None, None).await;
    fixture.conversation("inherited", Some(None), None).await;
    let narrowed = fixture
        .conversation("narrowed", Some(Some(vec![])), None)
        .await;
    let update = || UpdatePermissions {
        approval_mode: ApprovalMode::AutoReview,
        permission_mode: Some(PermissionMode::WorkspaceWrite),
    };
    assert!(matches!(
        fixture
            .state
            .update_permissions("inherited", update())
            .await,
        Err(ServiceError::Conflict(_))
    ));
    narrowed.background.store(true, Ordering::Release);
    assert!(matches!(
        fixture.state.update_permissions("parent", update()).await,
        Err(ServiceError::Conflict(_))
    ));
    narrowed.background.store(false, Ordering::Release);
    let result = fixture
        .state
        .update_permissions("parent", update())
        .await
        .unwrap();
    assert_eq!(result.approval_mode, ApprovalMode::AutoReview);
    assert!(result.editable);
    for id in ["parent", "inherited", "narrowed"] {
        let entry = fixture.state.entry(id).unwrap();
        assert_eq!(
            entry.workspace.as_ref().unwrap().approval_mode().unwrap(),
            ApprovalMode::AutoReview
        );
        assert_eq!(
            entry.snapshot.get().summary.permission_mode,
            if id == "narrowed" {
                PermissionMode::ReadOnly
            } else {
                PermissionMode::WorkspaceWrite
            }
        );
    }
    let tools = fixture
        .state
        .entry("narrowed")
        .unwrap()
        .workspace
        .clone()
        .unwrap();
    assert!(
        tools
            .execute(ToolCall {
                id: "cannot-widen".into(),
                name: "write_file".into(),
                arguments: json!({"path":"unexpected.txt", "content":"no"}).to_string()
            })
            .await
            .is_err()
    );
    fixture.close().await;
}

#[tokio::test]
async fn failed_full_access_commit_leaves_root_and_children_restricted() {
    let fixture = Fixture::new().await;
    let root = fixture.conversation("parent", None, None).await;
    let child = fixture.conversation("inherited", Some(None), None).await;
    let snapshot = fixture.directory.join("data/runs/parent.json");
    let saved = snapshot.with_extension("saved");
    fs::rename(&snapshot, &saved).unwrap();
    fs::create_dir(&snapshot).unwrap();
    assert!(
        fixture
            .state
            .update_permissions(
                "parent",
                UpdatePermissions {
                    approval_mode: ApprovalMode::FullAccess,
                    permission_mode: None,
                }
            )
            .await
            .is_err()
    );
    fs::remove_dir(&snapshot).unwrap();
    fs::rename(&saved, &snapshot).unwrap();
    assert_eq!(
        fixture
            .state
            .run_permissions("parent")
            .unwrap()
            .approval_mode,
        ApprovalMode::Ask
    );
    for entry in [root, child] {
        let tools = entry.workspace.as_ref().unwrap();
        assert!(!tools.full_access());
        assert_eq!(tools.approval_mode().unwrap(), ApprovalMode::Ask);
    }
    fixture.close().await;
}

#[tokio::test]
async fn full_access_is_explicit_inherited_persisted_and_reversible() {
    let fixture = Fixture::new().await;
    fixture.conversation("parent", None, None).await;
    fixture.conversation("inherited", Some(None), None).await;
    fixture
        .conversation("narrowed", Some(Some(vec![])), None)
        .await;
    fixture.conversation("unrelated", None, None).await;
    let input: UpdatePermissions = serde_json::from_value(json!({
        "approval_mode": "full_access"
    }))
    .unwrap();
    let changed = fixture
        .state
        .update_permissions("parent", input)
        .await
        .unwrap();
    assert_eq!(changed.approval_mode, ApprovalMode::FullAccess);
    assert_eq!(changed.permission_mode, PermissionMode::WorkspaceWrite);
    for id in ["parent", "inherited", "narrowed"] {
        assert_eq!(
            fixture.state.run_permissions(id).unwrap().approval_mode,
            ApprovalMode::FullAccess
        );
    }
    assert_eq!(
        fixture
            .state
            .run_permissions("unrelated")
            .unwrap()
            .approval_mode,
        ApprovalMode::Ask
    );
    assert_eq!(
        fixture
            .state
            .get("narrowed")
            .unwrap()
            .summary
            .permission_mode,
        PermissionMode::ReadOnly
    );
    let outside = fixture.directory.join("outside.txt");
    let write = || {
        ToolCall {
        id: "outside-write".into(),
        name: "write_file".into(),
        arguments: json!({"path": outside, "expected_content": fs::read_to_string(&outside).ok(), "content": "full-access-fixture"}).to_string(),
    }
    };
    fixture
        .state
        .entry("parent")
        .unwrap()
        .workspace
        .as_ref()
        .unwrap()
        .execute(write())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "full-access-fixture");
    assert!(
        fixture
            .state
            .entry("narrowed")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .execute(write())
            .await
            .is_err()
    );
    assert!(
        fixture
            .state
            .entry("unrelated")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .execute(write())
            .await
            .is_err()
    );
    // Clearing individual grants must not falsely claim to revoke Full Access.
    assert!(
        fixture
            .state
            .revoke_permissions("parent", false)
            .await
            .is_err()
    );
    fixture.state.shutdown().await;
    let restored = AppState::open_with_delay(fixture.directory.join("data"), None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        restored.run_permissions("parent").unwrap().approval_mode,
        ApprovalMode::FullAccess
    );
    assert_eq!(
        restored.run_permissions("unrelated").unwrap().approval_mode,
        ApprovalMode::Ask
    );
    let changed = restored.revoke_permissions("parent", true).await.unwrap();
    assert_eq!(changed.approval_mode, ApprovalMode::Ask);
    for id in ["parent", "inherited", "narrowed"] {
        assert_eq!(
            restored.run_permissions(id).unwrap().approval_mode,
            ApprovalMode::Ask
        );
        assert!(
            restored
                .entry(id)
                .unwrap()
                .workspace
                .as_ref()
                .unwrap()
                .execute(write())
                .await
                .is_err()
        );
    }
    restored.shutdown().await;
    fixture.close().await;
}

#[cfg(any(target_os = "macos", windows))]
fn request(tools: &LocalTools) -> tokio::task::JoinHandle<std::io::Result<String>> {
    let tools = tools.clone();
    tokio::spawn(async move {
        tools.execute(ToolCall { id: "permission-request".into(), name: "run_command".into(), arguments: json!({
            "command": if cfg!(windows) { "Write-Output permission-test" } else { "printf permission-test" },
            "access":"read_only", "network_access":"allowed", "timeout_seconds":5, "yield_time_ms":1000,
        }).to_string() }).await
    })
}

#[cfg(any(target_os = "macos", windows))]
async fn pending(tools: &LocalTools) -> agent_local::PendingApproval {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(pending) = tools.pending_approvals().unwrap().into_iter().next() {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("command approval did not appear")
}

#[cfg(any(target_os = "macos", windows))]
async fn finish(request: tokio::task::JoinHandle<std::io::Result<String>>) {
    // The test validates authorization state, not native process availability.
    // In a nested CI sandbox the approved harmless shell may fail closed.
    let _ = tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn always_command_grant_is_conversation_bound_restored_and_revocable() {
    let fixture = Fixture::new().await;
    let parent = fixture.conversation("parent", None, None).await;
    fixture.conversation("child", Some(None), None).await;
    fixture.conversation("unrelated", None, None).await;
    let tools = parent.workspace.as_ref().unwrap();
    let task = request(tools);
    let approval = pending(tools).await;
    fixture
        .state
        .decide_command("parent", &approval.id, true, CommandDecisionScope::Always)
        .await
        .unwrap();
    finish(task).await;
    let grant = CommandGrant {
        access: CommandAccess::ReadOnly,
        network_access: NetworkAccess::Allowed,
    };
    assert_eq!(
        fixture
            .state
            .run_permissions("parent")
            .unwrap()
            .persistent_command_grants,
        vec![grant]
    );
    assert_eq!(
        fixture
            .state
            .entry("child")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants,
        vec![grant]
    );
    assert!(
        fixture
            .state
            .entry("unrelated")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants
            .is_empty()
    );
    fixture.state.shutdown().await;
    let restarted = AppState::open_with_delay(fixture.directory.join("data"), None, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        restarted
            .entry("parent")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants,
        vec![grant]
    );
    assert_eq!(
        restarted
            .entry("child")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants,
        vec![grant]
    );
    assert!(
        restarted
            .entry("unrelated")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants
            .is_empty()
    );
    restarted.revoke_permissions("parent", false).await.unwrap();
    assert!(
        restarted
            .entry("child")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants
            .is_empty()
    );
    restarted.shutdown().await;
    let reopened = AppState::open_with_delay(fixture.directory.join("data"), None, Duration::ZERO)
        .await
        .unwrap();
    assert!(
        reopened
            .run_permissions("parent")
            .unwrap()
            .persistent_command_grants
            .is_empty()
    );
    assert!(
        reopened
            .entry("parent")
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .review()
            .unwrap()
            .command_grants
            .is_empty()
    );
    reopened.shutdown().await;
    fixture.close().await;
}

#[cfg(any(target_os = "macos", windows))]
async fn review_result(state: &AppState, status: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state
                .run_permissions("parent")
                .unwrap()
                .reviews
                .iter()
                .any(|r| r.status == status)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("review did not settle");
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn unavailable_automatic_reviewer_leaves_request_for_human_confirmation() {
    let fixture = Fixture::new().await;
    fixture.conversation("parent", None, None).await;
    fixture
        .state
        .update_permissions(
            "parent",
            UpdatePermissions {
                approval_mode: ApprovalMode::AutoReview,
                permission_mode: Some(PermissionMode::WorkspaceWrite),
            },
        )
        .await
        .unwrap();
    let tools = fixture
        .state
        .entry("parent")
        .unwrap()
        .workspace
        .clone()
        .unwrap();
    let task = request(&tools);
    let approval = pending(&tools).await;
    let state = fixture.state.clone();
    let reviewer = tokio::spawn(async move { state.review_pending_loop("parent").await });
    review_result(&fixture.state, "manual").await;
    assert_eq!(tools.pending_approvals().unwrap().len(), 1);
    assert!(tools.review().unwrap().command_grants.is_empty());
    assert!(
        fixture
            .state
            .run_permissions("parent")
            .unwrap()
            .persistent_command_grants
            .is_empty()
    );
    fixture
        .state
        .decide_command("parent", &approval.id, false, CommandDecisionScope::Once)
        .await
        .unwrap();
    finish(task).await;
    reviewer.abort();
    let _ = reviewer.await;
    fixture.close().await;
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn independent_low_risk_review_only_allows_the_exact_pending_command_once() {
    use axum::{Json, Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, Router::new().route("/v1/chat/completions", post(|| async {
        Json(json!({"id":"review-fixture", "choices":[{"message":{"role":"assistant", "content":r#"{"decision":"allow","risk":"low","authorized":true,"reason":"仅检查用户请求的公开资料"}"#},"finish_reason":"stop"}], "usage":{"prompt_tokens":10,"completion_tokens":10}}))
    }))).into_future());
    let fixture = Fixture::new().await;
    let model = fixture
        .state
        .configure_model(ConfigureModel {
            protocol: agent_providers::ProviderProtocol::ChatCompletions,
            base_url,
            api_key: "fixture".into(),
            model: "review-test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            reasoning_mode: "off".into(),
            reasoning_effort: "medium".into(),
            reasoning_capability: None,
            reasoning_default: None,
            supports_images: false,
        })
        .await
        .unwrap();
    fixture.conversation("parent", None, Some(&model.id)).await;
    fixture
        .state
        .update_permissions(
            "parent",
            UpdatePermissions {
                approval_mode: ApprovalMode::AutoReview,
                permission_mode: Some(PermissionMode::WorkspaceWrite),
            },
        )
        .await
        .unwrap();
    let tools = fixture
        .state
        .entry("parent")
        .unwrap()
        .workspace
        .clone()
        .unwrap();
    let task = request(&tools);
    pending(&tools).await;
    let state = fixture.state.clone();
    let reviewer = tokio::spawn(async move { state.review_pending_loop("parent").await });
    review_result(&fixture.state, "approved").await;
    reviewer.abort();
    let _ = reviewer.await;
    finish(task).await;
    assert!(tools.review().unwrap().command_grants.is_empty());
    assert!(
        fixture
            .state
            .run_permissions("parent")
            .unwrap()
            .persistent_command_grants
            .is_empty()
    );
    let next = request(&tools);
    let approval = pending(&tools).await;
    fixture
        .state
        .decide_command("parent", &approval.id, false, CommandDecisionScope::Once)
        .await
        .unwrap();
    finish(next).await;
    server.abort();
    let _ = server.await;
    fixture.close().await;
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn late_automatic_review_cannot_restore_a_revoked_command_permission() {
    use axum::{Json, Router, routing::post};
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let notify_started = started.clone();
    let wait_release = release.clone();
    let server = tokio::spawn(axum::serve(listener, Router::new().route("/v1/chat/completions", post(move || {
        let started = notify_started.clone();
        let release = wait_release.clone();
        async move {
            started.notify_one();
            release.notified().await;
            Json(json!({"id":"late-review-fixture", "choices":[{"message":{"role":"assistant", "content":r#"{"decision":"allow","risk":"low","authorized":true,"reason":"仅检查公开资料"}"#},"finish_reason":"stop"}]}))
        }
    }))).into_future());
    let fixture = Fixture::new().await;
    let model = fixture
        .state
        .configure_model(ConfigureModel {
            protocol: agent_providers::ProviderProtocol::ChatCompletions,
            base_url,
            api_key: "fixture".into(),
            model: "late-review-test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            reasoning_mode: "off".into(),
            reasoning_effort: "medium".into(),
            reasoning_capability: None,
            reasoning_default: None,
            supports_images: false,
        })
        .await
        .unwrap();
    fixture.conversation("parent", None, Some(&model.id)).await;
    fixture
        .state
        .update_permissions(
            "parent",
            UpdatePermissions {
                approval_mode: ApprovalMode::AutoReview,
                permission_mode: Some(PermissionMode::WorkspaceWrite),
            },
        )
        .await
        .unwrap();
    let tools = fixture
        .state
        .entry("parent")
        .unwrap()
        .workspace
        .clone()
        .unwrap();
    let task = request(&tools);
    pending(&tools).await;
    let state = fixture.state.clone();
    let reviewer = tokio::spawn(async move { state.review_pending_loop("parent").await });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    fixture
        .state
        .revoke_permissions("parent", false)
        .await
        .unwrap();
    release.notify_one();
    review_result(&fixture.state, "cancelled").await;
    finish(task).await;
    assert!(tools.pending_approvals().unwrap().is_empty());
    assert!(tools.review().unwrap().command_grants.is_empty());
    assert!(
        fixture
            .state
            .run_permissions("parent")
            .unwrap()
            .persistent_command_grants
            .is_empty()
    );
    reviewer.abort();
    let _ = reviewer.await;
    server.abort();
    let _ = server.await;
    fixture.close().await;
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::test]
async fn resumed_model_turn_keeps_auto_review_after_switching_to_read_only() {
    use axum::{Json, Router, response::IntoResponse, routing::post};
    use serde_json::Value;
    let reviews = Arc::new(AtomicU64::new(0));
    let observed_reviews = reviews.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let reviews = observed_reviews.clone();
        async move {
            let messages = body["messages"].as_array().unwrap();
            let user_index = messages.iter().rposition(|m| m["role"] == "user").unwrap();
            let prompt = messages[user_index]["content"].as_str().unwrap();
            let reviewing = serde_json::from_str::<Value>(prompt).ok().is_some_and(|v| v.get("proposed_action").is_some());
            let tool = !reviewing && prompt != "INITIAL_IDLE" && !messages[user_index + 1..].iter().any(|m| m["role"] == "tool");
            let mut message = json!({"role":"assistant"});
            if reviewing {
                reviews.fetch_add(1, Ordering::Relaxed);
                assert!(body["tools"].as_array().is_none_or(Vec::is_empty));
                message["content"] = json!(r#"{"decision":"allow","risk":"low","authorized":true,"reason":"只输出固定测试标记"}"#);
            } else if tool {
                message["tool_calls"] = json!([{"id":format!("call-{prompt}"),"type":"function", "function": {
                    "name":"run_command", "arguments":json!({"command":format!("echo {prompt}"),"access":"read_only","network_access":"allowed","timeout_seconds":5}).to_string()
                }}]);
            } else {
                message["content"] = json!(format!("Finished {prompt}"));
            }
            let reason = if tool { "tool_calls" } else { "stop" };
            if body["stream"] != true {
                return Json(json!({"id":"mode-review-response","choices":[{"index":0,"message":message,"finish_reason":reason}],"usage":{"prompt_tokens":100,"completion_tokens":20}})).into_response();
            }
            if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for (index, call) in calls.iter_mut().enumerate() { call["index"] = json!(index); }
            }
            let event = json!({"id":"mode-followup-response","choices":[{"index":0,"delta":message,"finish_reason":reason}]});
            ([("content-type", "text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n")).into_response()
        }
    }))).into_future());
    let fixture = Fixture::new().await;
    fixture
        .state
        .set_plugin_enabled("builtin.llm-wiki".into(), false)
        .await
        .unwrap();
    let model = fixture
        .state
        .configure_model(ConfigureModel {
            protocol: agent_providers::ProviderProtocol::ChatCompletions,
            base_url,
            api_key: "fixture".into(),
            model: "resumed-review-test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            reasoning_mode: "off".into(),
            reasoning_effort: "medium".into(),
            reasoning_capability: None,
            reasoning_default: None,
            supports_images: false,
        })
        .await
        .unwrap();
    let run = fixture
        .state
        .create(CreateRunInput {
            execution_policy: ExecutionPolicy::default(),
            prompt: "INITIAL_IDLE".into(),
            files: vec![],
            model_id: model.id,
            reasoning_effort: None,
            workspace_path: Some(fixture.project.display().to_string()),
            permission_mode: PermissionMode::WorkspaceWrite,
        })
        .await
        .unwrap();
    let id = &run.summary.id;
    async fn idle(state: &AppState, id: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let entry = state.entry(id).unwrap();
                let status = entry.snapshot.get().summary.status;
                assert!(
                    !matches!(
                        status,
                        RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted
                    ),
                    "unexpected outcome: {:?}",
                    entry.snapshot.get().error
                );
                if status == RunStatus::WaitingForInput && !entry.background.load(Ordering::Acquire)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    idle(&fixture.state, id).await;
    fixture
        .state
        .update_permissions(
            id,
            UpdatePermissions {
                approval_mode: ApprovalMode::AutoReview,
                permission_mode: Some(PermissionMode::ReadOnly),
            },
        )
        .await
        .unwrap();
    for (index, prompt) in ["NETWORK_AFTER_MODE_CHANGE", "NETWORK_SECOND_TURN"]
        .into_iter()
        .enumerate()
    {
        let submitted = fixture.state.input(id, prompt.into()).await.unwrap();
        assert_eq!(submitted.summary.status, RunStatus::Running);
        idle(&fixture.state, id).await;
        let review = fixture.state.workspace_review(id).await.unwrap();
        assert_eq!(review.commands.len(), index + 1);
        let command = review.commands.last().unwrap();
        assert_eq!(command.status, "completed", "{command:?}");
        assert_eq!(command.access, CommandAccess::ReadOnly);
        assert!(command.output.contains(prompt), "{command:?}");
        assert!(review.approvals.is_empty());
        assert!(review.command_grants.is_empty());
        let permissions = fixture.state.run_permissions(id).unwrap();
        assert_eq!(permissions.reviews.len(), index + 1);
        assert!(permissions.reviews.iter().all(|r| r.status == "approved"));
    }
    assert_eq!(reviews.load(Ordering::Relaxed), 2);
    server.abort();
    let _ = server.await;
    fixture.close().await;
}
