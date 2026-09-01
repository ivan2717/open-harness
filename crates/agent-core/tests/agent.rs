use agent_core::agent::{AgentRun, AgentRunError, AgentRunHistoryError, RunEvent, RunStatus};

fn test_run() -> AgentRun {
    AgentRun::new("run-1", "agent-1", "Research Rust ownership")
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
    run.record(RunEvent::InputRequested {
        prompt: "Which repository should I inspect?".to_owned(),
    })
    .unwrap();

    assert_eq!(run.status(), RunStatus::WaitingForInput);

    run.record(RunEvent::InputReceived {
        content: "open-harness".to_owned(),
    })
    .unwrap();

    assert_eq!(run.status(), RunStatus::Running);
}

#[test]
fn a_terminal_run_rejects_more_events() {
    let mut run = test_run();
    run.begin_step().unwrap();
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
            content: "open-harness".to_owned(),
        })
        .unwrap();
    original.begin_step().unwrap();
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
