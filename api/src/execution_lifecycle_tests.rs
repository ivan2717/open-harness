use super::*;
use crate::contracts::Delegation;
use agent_runtime::ToolExecutor;

struct Fixture {
    directory: PathBuf,
    state: AppState,
    parent: Arc<RunEntry>,
    child: Arc<RunEntry>,
    access: agent_local::WorkspaceAccess,
    events: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-execution-lifecycle-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let project = directory.join("project");
        fs::create_dir_all(&project).unwrap();
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let mut entries = Vec::new();
        for id in ["parent", "child"] {
            let now = time::now();
            let detail = RunDetail {
                summary: RunSummary {
                    reasoning_settings_version: 1,
                    reasoning_effort: None,
                    model_history_through: None,
                    delegation: (id == "child").then(|| Delegation {
                        parent_run_id: "parent".into(),
                        root_run_id: "parent".into(),
                        task_name: "child".into(),
                        write_paths: vec!["owned.txt".into()],
                        spawn_call_id: "spawn-child".into(),
                    }),
                    execution_policy: Default::default(),
                    compacting: false,
                    id: id.into(),
                    revision: 0,
                    title: id.into(),
                    prompt: id.into(),
                    status: RunStatus::Running,
                    // Host collaboration hooks are active; no network model is called.
                    model_id: "lifecycle-fixture".into(),
                    workspace_path: Some(project.to_string_lossy().into_owned()),
                    permission_mode: agent_local::PermissionMode::WorkspaceWrite,
                    created_at: now.clone(),
                    updated_at: now,
                },
                messages: Vec::new(),
                events: Vec::new(),
                context: Some(initial_context()),
                error: None,
            };
            let entry = state.make_entry(detail, 0).unwrap();
            entry.runtime.start_run(&entry.agent, id, id).await.unwrap();
            state
                .inner
                .runs
                .write()
                .unwrap()
                .insert(id.into(), entry.clone());
            entries.push(entry);
        }
        let access = state.workspace_access(project.to_str().unwrap()).unwrap();
        access.try_acquire("child", &["owned.txt".into()]).unwrap();
        entries[1].background.store(true, Ordering::Release);
        Self {
            events: directory.join("events/run-6368696c64.jsonl"),
            directory,
            state,
            parent: entries.remove(0),
            child: entries.remove(0),
            access,
        }
    }

    async fn verify(&self, expected_error: &str) {
        let detail = self.child.snapshot.get();
        assert_eq!(detail.summary.status, RunStatus::Interrupted);
        assert!(detail.error.as_ref().unwrap().contains(expected_error));
        assert!(
            detail
                .error
                .as_ref()
                .unwrap()
                .contains("部分操作可能已经生效")
        );
        assert!(!self.child.background.load(Ordering::Acquire));
        assert!(!self.child.runtime.is_executing("child"));
        self.access
            .try_acquire("next-owner", &["owned.txt".into()])
            .unwrap();
        self.access.release("next-owner").await;
    }

    async fn close(self) {
        self.state.shutdown().await;
        fs::remove_dir_all(self.directory).unwrap();
    }
}

async fn exercise_failure(
    fixture: &Fixture,
    execution: impl std::future::Future<Output = io::Result<CoreStatus>>,
) {
    let parent = fixture.parent.clone();
    let waiting = tokio::spawn(async move { parent.runtime.tools().wait_for_messages().await });
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "coordinator must initially wait for its child"
    );

    execute_and_settle(&fixture.state, &fixture.child, "child", execution).await;

    let messages = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("coordinator must not hang after a worker failure")
        .unwrap()
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("Interrupted"));
    assert!(messages[0].text.contains("部分操作可能已经生效"));
    // The failure outcome is durable, not merely a notification to this waiter.
    assert_eq!(
        fixture
            .parent
            .runtime
            .tools()
            .pending_messages()
            .await
            .unwrap()[0]
            .id,
        messages[0].id
    );
}

#[tokio::test]
async fn execution_panic_reports_interruption_and_releases_child_ownership() {
    let fixture = Fixture::new().await;
    exercise_failure(
        &fixture,
        std::future::poll_fn(|_| -> std::task::Poll<io::Result<CoreStatus>> {
            panic!("injected execution panic");
        }),
    )
    .await;
    fixture.verify("内部异常").await;
    fixture.close().await;
}

#[tokio::test]
async fn missing_or_unreadable_final_history_does_not_strand_running_child() {
    for unreadable in [false, true] {
        let fixture = Fixture::new().await;
        let backup = fixture.events.with_extension("backup");
        fs::rename(&fixture.events, &backup).unwrap();
        if unreadable {
            fs::create_dir(&fixture.events).unwrap();
        }
        exercise_failure(&fixture, async { Ok(CoreStatus::Running) }).await;
        fixture
            .verify(if unreadable {
                "无法核实"
            } else {
                "未找到"
            })
            .await;
        if unreadable {
            fs::remove_dir(&fixture.events).unwrap();
        }
        fs::rename(backup, &fixture.events).unwrap();
        fixture.close().await;
    }
}

#[tokio::test]
async fn shutdown_preserves_idle_outcomes_and_waits_for_worker_cleanup_without_operation_locks() {
    use agent_core::agent::{RunEvent, RunPauseReason};
    for paused in [false, true] {
        let fixture = Fixture::new().await;
        for entry in [&fixture.parent, &fixture.child] {
            let id = entry.snapshot.get().summary.id;
            let mut run = entry.runtime.restore_run(&id).await.unwrap().unwrap();
            if paused {
                entry
                    .runtime
                    .record_event(
                        &mut run,
                        RunEvent::RunPaused {
                            reason: RunPauseReason::StepLimit { max_steps: 2 },
                        },
                    )
                    .await
                    .unwrap();
            } else {
                run.begin_step().unwrap();
                entry
                    .runtime
                    .record_event(
                        &mut run,
                        RunEvent::ModelResponseReceived {
                            content: "awaiting input".into(),
                            response: Some(agent_runtime::ModelResponse::input_required(
                                "awaiting input".into(),
                            )),
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
                            prompt: "awaiting input".into(),
                        },
                    )
                    .await
                    .unwrap();
            }
        }
        let expected = if paused {
            RunStatus::Paused
        } else {
            RunStatus::WaitingForInput
        };
        let state = fixture.state.clone();
        let mut closing = tokio::spawn(async move { state.shutdown().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut closing)
                .await
                .is_err(),
            "shutdown must wait for the worker's post-outcome bookkeeping"
        );
        assert_eq!(fixture.child.snapshot.get().summary.status, expected);
        // A coordinator's cleanup needs this lock to stop a child. Shutdown
        // cannot hold it while waiting for that same coordinator to finish.
        let operation =
            tokio::time::timeout(Duration::from_secs(1), fixture.child.operations.lock())
                .await
                .expect("shutdown must release operation locks before draining workers");
        drop(operation);
        let gate = tokio::time::timeout(
            Duration::from_secs(1),
            fixture.state.inner.model_gate.lock(),
        )
        .await
        .expect("queued admissions must be able to observe shutdown");
        assert!(fixture.state.ensure_accepting_work().is_err());
        drop(gate);
        execute_and_settle(&fixture.state, &fixture.child, "child", async {
            Ok::<_, io::Error>(if paused {
                CoreStatus::Paused
            } else {
                CoreStatus::WaitingForInput
            })
        })
        .await;
        tokio::time::timeout(Duration::from_secs(1), closing)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fixture.child.snapshot.get().summary.status, expected);
        assert!(!fixture.child.background.load(Ordering::Acquire));
        let reopened = AppState::open_with_delay(fixture.directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(reopened.get("parent").unwrap().summary.status, expected);
        assert_eq!(reopened.get("child").unwrap().summary.status, expected);
        reopened.shutdown().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn revert_waits_for_worker_tail_and_rechecks_family_after_releasing_locks() {
    for resumed in [false, true] {
        let fixture = Fixture::new().await;
        let tools = fixture.child.workspace.as_ref().unwrap();
        tools
            .execute(agent_runtime::ToolCall {
                id: "child-edit".into(),
                name: "apply_patch".into(),
                arguments: serde_json::json!({
                    "patch": "*** Begin Patch\n*** Add File: owned.txt\n+child result\n*** End Patch"
                })
                .to_string(),
            })
            .await
            .unwrap();
        for entry in [&fixture.parent, &fixture.child] {
            entry
                .snapshot
                .update(|detail| detail.summary.status = RunStatus::WaitingForInput)
                .unwrap();
        }
        let state = fixture.state.clone();
        let mut reverting =
            tokio::spawn(async move { state.revert_file("child", "owned.txt").await });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut reverting)
                .await
                .is_err(),
            "a published outcome must wait for the worker's lease cleanup"
        );
        assert!(tools.root().join("owned.txt").exists());

        // Pending undo must not block cancellation or fresh admission. A new
        // turn admitted while waiting must be observed before undo can proceed.
        let gate = tokio::time::timeout(
            Duration::from_secs(1),
            fixture.state.inner.model_gate.lock(),
        )
        .await
        .expect("undo must release the admission gate while awaiting cleanup");
        let operation =
            tokio::time::timeout(Duration::from_secs(1), fixture.child.operations.lock())
                .await
                .expect("undo must release operation locks while awaiting cleanup");
        if resumed {
            fixture
                .parent
                .snapshot
                .update(|detail| detail.summary.status = RunStatus::Running)
                .unwrap();
        }
        fixture.access.release("child").await;
        drop(BackgroundExecution {
            state: &fixture.state,
            entry: &fixture.child,
        });
        drop(operation);
        drop(gate);

        let result = tokio::time::timeout(Duration::from_secs(1), reverting)
            .await
            .expect("worker cleanup must wake pending undo")
            .unwrap();
        if resumed {
            assert!(matches!(result, Err(ServiceError::Conflict(_))));
            assert_eq!(
                fs::read_to_string(tools.root().join("owned.txt")).unwrap(),
                "child result\n"
            );
        } else {
            assert!(result.unwrap().changes.is_empty());
            assert!(!tools.root().join("owned.txt").exists());
        }
        fixture.close().await;
    }
}
