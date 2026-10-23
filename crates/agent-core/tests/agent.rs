use agent_core::agent::{
    AgentRun, AgentRunError, AgentRunHistoryError, RunEvent, RunEventEnvelope, RunStatus,
};

fn test_run() -> AgentRun {
    AgentRun::new("run-1", "agent-1", "Research Rust ownership")
}

fn respond(run: &mut AgentRun) {
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "response".to_owned(),
    })
    .unwrap();
}

fn request(call_id: &str) -> RunEvent {
    RunEvent::ToolCallRequested {
        call_id: call_id.to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: "{}".to_owned(),
    }
}

fn succeed(call_id: &str) -> RunEvent {
    RunEvent::ToolCallSucceeded {
        call_id: call_id.to_owned(),
        output: "contents".to_owned(),
        images: Vec::new(),
    }
}

fn fail(call_id: &str) -> RunEvent {
    RunEvent::ToolCallFailed {
        call_id: call_id.to_owned(),
        message: "unavailable".to_owned(),
    }
}

fn complete() -> RunEvent {
    RunEvent::RunCompleted {
        answer: "finished".to_owned(),
    }
}

fn assert_rejected_without_changes(run: &mut AgentRun, event: RunEvent) {
    let original_events = run.events().to_vec();
    let original_status = run.status();
    let original_step = run.current_step();
    run.record(event).unwrap_err();
    assert_eq!(run.events(), original_events);
    assert_eq!(run.status(), original_status);
    assert_eq!(run.current_step(), original_step);
}

#[test]
fn a_new_run_starts_with_one_event() {
    let run = test_run();

    assert_eq!(run.status(), RunStatus::Running);
    assert_eq!(run.instruction(), "Research Rust ownership");
    assert_eq!(run.events().len(), 1);
    assert_eq!(run.events()[0].sequence, 0);
    assert_eq!(run.events()[0].step, 0);
    assert!(matches!(run.events()[0].event, RunEvent::RunStarted { .. }));
}

#[test]
fn events_are_ordered_and_grouped_by_step() {
    let mut run = test_run();

    assert_eq!(run.begin_step().unwrap(), 1);
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "I need to inspect the project.".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "call-1".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: "{\"path\":\"Cargo.toml\"}".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "call-1".to_owned(),
        output: "[workspace]".to_owned(),
        images: Vec::new(),
    })
    .unwrap();

    assert_eq!(run.events().len(), 5);
    assert_eq!(run.events()[4].sequence, 4);
    assert!(run.events()[1..].iter().all(|event| event.step == 1));
    assert_eq!(run.status(), RunStatus::Running);
}

#[test]
fn waiting_for_input_can_resume() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    run.record(RunEvent::InputRequested {
        prompt: "Which repository should I inspect?".to_owned(),
    })
    .unwrap();

    assert_eq!(run.status(), RunStatus::WaitingForInput);

    run.record(RunEvent::InputReceived {
        files: Vec::new(),
        content: "open-harness".to_owned(),
    })
    .unwrap();

    assert_eq!(run.status(), RunStatus::Running);
}

#[test]
fn a_terminal_run_rejects_more_events() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    run.record(RunEvent::RunCompleted {
        answer: "Ownership gives each value one owner.".to_owned(),
    })
    .unwrap();

    let error = run.record(RunEvent::RunCancelled).unwrap_err();

    assert_eq!(
        error,
        AgentRunError::RunAlreadyFinished {
            status: RunStatus::Completed,
        }
    );
}

#[test]
fn step_events_require_begin_step() {
    let mut run = test_run();

    let error = run
        .record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "response".to_owned(),
        })
        .unwrap_err();

    assert_eq!(
        error,
        AgentRunError::NoActiveStep {
            event: "ModelResponseReceived",
        }
    );
}

#[test]
fn input_received_requires_a_started_step() {
    let mut run = test_run();

    let error = run
        .record(RunEvent::InputReceived {
            files: Vec::new(),
            content: "open-harness".to_owned(),
        })
        .unwrap_err();

    assert_eq!(
        error,
        AgentRunError::NoActiveStep {
            event: "InputReceived",
        }
    );
}

#[test]
fn failure_and_cancellation_do_not_require_a_started_step() {
    let mut failed_run = test_run();
    failed_run
        .record(RunEvent::RunFailed {
            message: "model configuration is invalid".to_owned(),
        })
        .unwrap();

    let mut cancelled_run = test_run();
    cancelled_run.record(RunEvent::RunCancelled).unwrap();

    assert_eq!(failed_run.status(), RunStatus::Failed);
    assert_eq!(failed_run.events().last().unwrap().step, 0);
    assert_eq!(cancelled_run.status(), RunStatus::Cancelled);
    assert_eq!(cancelled_run.events().last().unwrap().step, 0);
}

#[test]
fn an_event_history_restores_the_run_projection() {
    let mut original = test_run();
    original.begin_step().unwrap();
    original
        .record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "I need the repository name.".to_owned(),
        })
        .unwrap();
    original
        .record(RunEvent::InputRequested {
            prompt: "Which repository?".to_owned(),
        })
        .unwrap();
    original
        .record(RunEvent::InputReceived {
            files: Vec::new(),
            content: "open-harness".to_owned(),
        })
        .unwrap();
    original.begin_step().unwrap();
    respond(&mut original);
    original
        .record(RunEvent::RunCompleted {
            answer: "I inspected open-harness.".to_owned(),
        })
        .unwrap();

    let restored = AgentRun::restore(
        original.id(),
        original.agent_id(),
        original.events().to_vec(),
    )
    .unwrap();

    assert_eq!(restored.status(), RunStatus::Completed);
    assert_eq!(restored.current_step(), 2);
    assert_eq!(restored.instruction(), original.instruction());
    assert_eq!(restored.events(), original.events());
}

#[test]
fn restore_rejects_a_gap_in_event_sequence() {
    let mut original = test_run();
    original.begin_step().unwrap();
    let mut events = original.events().to_vec();
    events[1].sequence = 2;

    let error = AgentRun::restore("run-1", "agent-1", events).unwrap_err();

    assert_eq!(
        error,
        AgentRunHistoryError::UnexpectedSequence {
            index: 1,
            expected: 1,
            actual: 2,
        }
    );
}

#[test]
fn restore_rejects_an_event_from_the_wrong_step() {
    let mut original = test_run();
    original.begin_step().unwrap();
    original
        .record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "response".to_owned(),
        })
        .unwrap();
    let mut events = original.events().to_vec();
    events[2].step = 2;

    let error = AgentRun::restore("run-1", "agent-1", events).unwrap_err();

    assert_eq!(
        error,
        AgentRunHistoryError::UnexpectedStep {
            sequence: 2,
            event: "ModelResponseReceived",
            expected: 1,
            actual: 2,
        }
    );
}

#[test]
fn a_model_response_must_arrive_before_the_step_can_finish() {
    let mut run = test_run();
    run.begin_step().unwrap();

    let original_events = run.events().to_vec();
    assert!(!run.can_begin_step());
    run.begin_step().unwrap_err();
    assert_eq!(run.current_step(), 1);
    assert_eq!(run.events(), original_events);
    assert_rejected_without_changes(&mut run, complete());
    assert_rejected_without_changes(&mut run, request("call-1"));
    assert_rejected_without_changes(
        &mut run,
        RunEvent::InputRequested {
            prompt: "choose".to_owned(),
        },
    );

    respond(&mut run);
    assert_rejected_without_changes(
        &mut run,
        RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "duplicate".to_owned(),
        },
    );
    // The response still needs a recorded decision before a new step is safe.
    assert!(!run.can_begin_step());
    run.record(complete()).unwrap();
}

#[test]
fn tool_results_must_match_an_unresolved_request_in_this_step() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    assert_rejected_without_changes(&mut run, succeed("unknown"));
    assert_rejected_without_changes(&mut run, fail("unknown"));

    run.record(request("call-1")).unwrap();
    run.record(request("call-2")).unwrap();
    assert!(run.has_tool_call("call-1"));
    assert_rejected_without_changes(&mut run, request("call-1"));
    assert_rejected_without_changes(&mut run, complete());
    assert!(!run.can_begin_step());
    run.begin_step().unwrap_err();

    // Results may arrive in a different order from their requests.
    run.record(fail("call-2")).unwrap();
    assert_rejected_without_changes(&mut run, succeed("call-2"));
    assert_rejected_without_changes(&mut run, request("call-3"));
    assert_rejected_without_changes(&mut run, complete());
    assert!(!run.can_begin_step());

    run.record(succeed("call-1")).unwrap();
    assert_rejected_without_changes(&mut run, fail("call-1"));
    assert!(run.can_begin_step());
    run.begin_step().unwrap();
    respond(&mut run);
    assert_rejected_without_changes(&mut run, succeed("call-1"));
    assert_rejected_without_changes(&mut run, request("call-1"));
    run.record(complete()).unwrap();
}

#[test]
fn input_cannot_interrupt_pending_tool_calls() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    run.record(request("call-1")).unwrap();

    assert_rejected_without_changes(
        &mut run,
        RunEvent::InputRequested {
            prompt: "choose".to_owned(),
        },
    );
    assert_rejected_without_changes(
        &mut run,
        RunEvent::InputReceived {
            files: Vec::new(),
            content: "choice".to_owned(),
        },
    );
}

#[test]
fn restoration_preserves_pending_calls_and_global_call_id_uniqueness() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    run.record(request("call-1")).unwrap();
    run.record(request("call-2")).unwrap();
    run.record(succeed("call-1")).unwrap();

    let mut restored = AgentRun::restore("run-1", "agent-1", run.events().to_vec()).unwrap();
    assert!(!restored.can_begin_step());
    assert!(restored.has_tool_call("call-1"));
    assert!(restored.has_tool_call("call-2"));
    assert_rejected_without_changes(&mut restored, fail("call-1"));
    restored.record(fail("call-2")).unwrap();
    assert!(restored.can_begin_step());
    restored.begin_step().unwrap();
    respond(&mut restored);
    assert_rejected_without_changes(&mut restored, request("call-2"));
}

#[test]
fn restoration_rejects_invalid_tool_lifecycles_and_incomplete_step_boundaries() {
    let mut run = test_run();
    run.begin_step().unwrap();
    let waiting_for_model = run.events().to_vec();
    respond(&mut run);
    let no_calls = run.events().to_vec();
    run.record(request("call-1")).unwrap();
    run.record(request("call-2")).unwrap();
    let pending_calls = run.events().to_vec();
    run.record(succeed("call-1")).unwrap();
    let resolving_calls = run.events().to_vec();
    run.record(fail("call-2")).unwrap();
    run.begin_step().unwrap();
    respond(&mut run);
    let next_step = run.events().to_vec();

    for (mut events, event) in [
        (waiting_for_model.clone(), complete()),
        (waiting_for_model, RunEvent::ModelRequestStarted),
        (no_calls, succeed("orphan")),
        (pending_calls.clone(), complete()),
        (pending_calls, RunEvent::ModelRequestStarted),
        (resolving_calls.clone(), fail("call-1")),
        (resolving_calls, request("call-3")),
        (next_step.clone(), succeed("call-2")),
        (next_step, request("call-1")),
    ] {
        let sequence = events.len() as u64;
        let step =
            events.last().unwrap().step + u32::from(matches!(event, RunEvent::ModelRequestStarted));
        events.push(RunEventEnvelope {
            sequence,
            step,
            occurred_at: std::time::SystemTime::now(),
            event,
        });
        assert!(matches!(
            AgentRun::restore("run-1", "agent-1", events),
            Err(AgentRunHistoryError::InvalidEvent {
                sequence: failed_sequence,
                ..
            }) if failed_sequence == sequence
        ));
    }
}

#[test]
fn preparing_events_does_not_change_the_run_until_commit() {
    let mut run = test_run();
    let prepared = run.prepare_begin_step().unwrap();
    assert_eq!(run.current_step(), 0);
    assert_eq!(run.events().len(), 1);
    assert_eq!(prepared.envelope().step, 1);
    let expected = prepared.envelope().clone();
    assert_eq!(run.commit_prepared(prepared).unwrap(), &expected);

    let prepared = run
        .prepare_event(RunEvent::RunFailed {
            message: "failed".to_owned(),
        })
        .unwrap();
    assert_eq!(run.status(), RunStatus::Running);
    assert_eq!(run.events().len(), 2);
    drop(prepared); // A persistence failure can discard the event safely.
    assert_eq!(run.status(), RunStatus::Running);
    respond(&mut run);
    run.record(complete()).unwrap();
}

#[test]
fn preparing_a_tool_event_leaves_call_indexes_unchanged_until_commit() {
    let mut run = test_run();
    run.begin_step().unwrap();
    respond(&mut run);
    let requested = run.prepare_event(request("call-1")).unwrap();
    assert!(!run.has_tool_call("call-1"));
    assert!(run.prepare_event(complete()).is_ok());
    run.commit_prepared(requested).unwrap();
    assert!(run.has_tool_call("call-1"));
    assert!(run.prepare_event(complete()).is_err());

    let succeeded = run.prepare_event(succeed("call-1")).unwrap();
    assert!(!run.can_begin_step());
    assert!(run.prepare_event(fail("call-1")).is_ok());
    run.commit_prepared(succeeded).unwrap();
    assert!(run.can_begin_step());
    assert!(run.prepare_event(fail("call-1")).is_err());
}

#[test]
fn prepared_events_reject_stale_runs_unrelated_runs_and_diverged_clones() {
    let mut run = test_run();
    let stale = run.prepare_begin_step().unwrap();
    let unrelated = run.prepare_begin_step().unwrap();
    let mut other_run = test_run();
    assert_eq!(
        other_run.commit_prepared(unrelated).unwrap_err(),
        AgentRunError::StalePreparedEvent
    );
    assert_eq!(other_run.events().len(), 1);

    let mut branch = run.clone();
    run.begin_step().unwrap();
    assert_eq!(
        run.commit_prepared(stale).unwrap_err(),
        AgentRunError::StalePreparedEvent
    );
    branch.begin_step().unwrap();
    let from_branch = branch
        .prepare_event(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "branch response".to_owned(),
        })
        .unwrap();
    assert_eq!(
        run.commit_prepared(from_branch).unwrap_err(),
        AgentRunError::StalePreparedEvent
    );
    assert_eq!(run.events().len(), 2);
    respond(&mut run);
}
