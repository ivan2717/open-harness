use super::*;
use agent_core::agent::RunEvent;
use agent_runtime::ModelResponse;

#[tokio::test]
async fn cancellation_reconciles_completion_or_failure_after_its_initial_read() {
    let directory = std::env::temp_dir().join(format!(
        "open-harness-cancel-race-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
        .await
        .unwrap();
    for completed in [true, false] {
        let id = format!("race-{completed}");
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                delegation: None,
                execution_policy: Default::default(),
                compacting: false,
                id: id.clone(),
                revision: 0,
                title: "race".into(),
                prompt: "race".into(),
                status: RunStatus::Running,
                model_id: MODEL_ID.into(),
                workspace_path: None,
                permission_mode: Default::default(),
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let entry = state.make_entry(detail, 0).unwrap();
        let stale = entry
            .runtime
            .start_run(&agent(), &id, "race")
            .await
            .unwrap();
        let mut winner = stale.clone();
        if completed {
            winner.begin_step().unwrap();
            entry
                .runtime
                .record_event(
                    &mut winner,
                    RunEvent::ModelResponseReceived {
                        content: "done".into(),
                        response: Some(ModelResponse::final_answer("done".into())),
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
                    &mut winner,
                    RunEvent::RunCompleted {
                        answer: "done".into(),
                    },
                )
                .await
                .unwrap();
        } else {
            entry
                .runtime
                .record_event(
                    &mut winner,
                    RunEvent::RunFailed {
                        message: "model failed first".into(),
                    },
                )
                .await
                .unwrap();
        }
        // Public cancel's initial read saw Running; its runtime cancellation
        // now sees the winning terminal commit and rejects RunCancelled.
        assert_eq!(stale.status(), CoreStatus::Running);
        let outcome = cancel_restored(&entry, stale).await.unwrap();
        assert_eq!(
            outcome.summary.status,
            if completed {
                RunStatus::Completed
            } else {
                RunStatus::Failed
            }
        );
        assert_eq!(outcome.events.len(), winner.events().len());
        assert!(
            !outcome
                .events
                .iter()
                .any(|event| event.r#type == "run_cancelled")
        );
        let again = cancel_restored(&entry, winner).await.unwrap();
        assert_eq!(again.summary.revision, outcome.summary.revision);
    }
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn shutdown_settles_active_work_and_rejects_new_execution() {
    let directory = std::env::temp_dir().join(format!(
        "open-harness-shutdown-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let state = AppState::open_with_delay(directory.clone(), None, Duration::from_millis(50))
        .await
        .unwrap();
    let input = || CreateRunInput {
        execution_policy: Default::default(),
        prompt: "shutdown".into(),
        files: Vec::new(),
        model_id: MODEL_ID.into(),
        workspace_path: None,
        permission_mode: Default::default(),
    };
    let created = state.create(input()).await.unwrap();
    state.shutdown().await;
    let stopped = state.get(&created.summary.id).unwrap();
    assert_eq!(stopped.summary.status, RunStatus::Cancelled);
    assert!(
        !state
            .entry(&created.summary.id)
            .unwrap()
            .runtime
            .is_executing(&created.summary.id)
    );
    assert!(state.create(input()).await.is_err());
    assert!(state.resume(&created.summary.id, None).await.is_err());
    // Closing twice is harmless and does not append duplicate cancellation.
    state.shutdown().await;
    assert_eq!(
        state.get(&created.summary.id).unwrap().events.len(),
        stopped.events.len()
    );
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn compact_waits_for_idle_worker_cleanup_and_rechecks_admission() {
    for blocker in ["none", "execution", "compaction", "shutdown"] {
        let directory = std::env::temp_dir().join(format!(
            "open-harness-compact-tail-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let created = state
            .create(CreateRunInput {
                execution_policy: Default::default(),
                prompt: "/ask compact cleanup".into(),
                files: Vec::new(),
                model_id: MODEL_ID.into(),
                workspace_path: None,
                permission_mode: Default::default(),
            })
            .await
            .unwrap();
        let id = &created.summary.id;
        let entry = state.entry(id).unwrap();
        let mut changed = state.inner.mailboxes.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while entry.background.load(Ordering::Acquire) || entry.runtime.is_executing(id) {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            entry.snapshot.get().summary.status,
            RunStatus::WaitingForInput
        );

        // Recreate the precise boundary: the idle outcome has been published,
        // but the worker still owns the execution tail until its guard drops.
        entry.background.store(true, Ordering::Release);
        let compact = state.compact(id);
        tokio::pin!(compact);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut compact)
                .await
                .is_err(),
            "manual compaction must wait for the published idle run's cleanup"
        );
        assert!(!entry.snapshot.get().summary.compacting);

        // Cleanup/cancellation and a competing admission must be able to take
        // both locks while compact is waiting.
        let gate = tokio::time::timeout(Duration::from_secs(1), state.inner.model_gate.lock())
            .await
            .unwrap();
        let operation = tokio::time::timeout(Duration::from_secs(1), entry.operations.lock())
            .await
            .unwrap();
        match blocker {
            "execution" => entry
                .snapshot
                .update(|detail| detail.summary.status = RunStatus::Running)
                .unwrap(),
            "compaction" => entry
                .snapshot
                .update(|detail| detail.summary.compacting = true)
                .unwrap(),
            "shutdown" => state.inner.shutting_down.store(true, Ordering::Release),
            _ => (),
        }
        drop(BackgroundExecution {
            state: &state,
            entry: &entry,
        });
        drop(operation);
        drop(gate);
        let result = tokio::time::timeout(Duration::from_secs(1), &mut compact)
            .await
            .unwrap();
        if blocker == "none" {
            result.unwrap();
        } else {
            assert!(
                matches!(result, Err(ServiceError::Conflict(_))),
                "{blocker}"
            );
            // Remove the simulated competing operation before cleanup.
            entry
                .snapshot
                .update(|detail| {
                    detail.summary.status = RunStatus::WaitingForInput;
                    detail.summary.compacting = false;
                })
                .unwrap();
        }
        state.shutdown().await;
        fs::remove_dir_all(directory).unwrap();
    }
}
