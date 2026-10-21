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
                id: id.clone(),
                revision: 0,
                title: "race".into(),
                prompt: "race".into(),
                status: RunStatus::Running,
                model_id: MODEL_ID.into(),
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
