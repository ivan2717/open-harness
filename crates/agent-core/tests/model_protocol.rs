use agent_core::{
    ContentBlock, MessageContext, ModelFinishReason, ModelMessage, ModelResponse, ModelRole,
    agent::{AgentRun, AgentRunError, AgentRunHistoryError, RunEvent, RunEventEnvelope, RunStatus},
    model_protocol::ModelProtocolError,
    provider_state::{
        ProviderContextIdentity, ProviderReplayScope, ProviderResponseState, ProviderToolCall,
    },
};
use serde_json::json;

fn awaiting_response() -> AgentRun {
    let mut run = AgentRun::new("canonical-run", "agent", "Inspect both files");
    run.begin_step().unwrap();
    run
}

fn calls() -> Vec<ProviderToolCall> {
    ["first", "second"]
        .into_iter()
        .map(|id| ProviderToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: format!(r#"{{"path":"{id}.rs"}}"#),
        })
        .collect()
}

fn response_event(response: ModelResponse) -> RunEvent {
    RunEvent::ModelResponseReceived {
        content: response.text(),
        response: Some(response),
        provider_state: None,
        provider_continuation: None,
    }
}

fn request(call: &ProviderToolCall) -> RunEvent {
    RunEvent::ToolCallRequested {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        arguments: call.arguments.clone(),
    }
}

fn assert_rejected_live_and_on_restore(
    run: &mut AgentRun,
    event: RunEvent,
    expected: AgentRunError,
) {
    let original = run.events().to_vec();
    assert_eq!(run.record(event.clone()).unwrap_err(), expected);
    assert_eq!(
        run.events(),
        original,
        "a rejected event must not mutate durable history"
    );
    let sequence = original.len() as u64;
    let mut corrupted = original;
    corrupted.push(RunEventEnvelope {
        occurred_at: std::time::SystemTime::UNIX_EPOCH,
        sequence,
        step: run.current_step(),
        event,
    });
    assert!(
        matches!(AgentRun::restore(run.id(), run.agent_id(), corrupted),
        Err(AgentRunHistoryError::InvalidEvent { sequence: actual, source })
            if actual == sequence && source == expected)
    );
}

#[test]
fn visible_response_mirror_must_match_ordered_text_on_record_and_restore() {
    let response = ModelResponse {
        content: vec![
            ContentBlock::Reasoning {
                text: "private deliberation".into(),
            },
            ContentBlock::Text {
                text: "First ".into(),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
        ],
        finish_reason: ModelFinishReason::Stop,
    };
    for content in [
        "another answer",
        "private deliberationFirst answer",
        "answerFirst ",
    ] {
        let mut run = awaiting_response();
        assert_rejected_live_and_on_restore(
            &mut run,
            RunEvent::ModelResponseReceived {
                content: content.into(),
                response: Some(response.clone()),
                provider_state: None,
                provider_continuation: None,
            },
            AgentRunError::InvalidCanonicalResponse,
        );
    }
    let mut run = awaiting_response();
    run.record(response_event(response)).unwrap();
    run.record(RunEvent::RunCompleted {
        answer: "First answer".into(),
    })
    .unwrap();
    let restored = AgentRun::restore(run.id(), run.agent_id(), run.events().to_vec()).unwrap();
    assert_eq!(restored.status(), RunStatus::Completed);
}

#[test]
fn tool_declarations_must_match_canonical_order_ids_names_and_arguments() {
    let expected = calls();
    for mutation in 0..4 {
        let mut run = awaiting_response();
        run.record(response_event(ModelResponse::tool_calls(
            "Inspecting".into(),
            expected.clone(),
        )))
        .unwrap();
        let mut changed = expected[0].clone();
        match mutation {
            0 => changed = expected[1].clone(),
            1 => changed.id = "substituted-id".into(),
            2 => changed.name = "write_file".into(),
            3 => changed.arguments = r#"{"path":"other.rs"}"#.into(),
            _ => unreachable!(),
        }
        assert_rejected_live_and_on_restore(
            &mut run,
            request(&changed),
            AgentRunError::CanonicalToolCallMismatch,
        );
        // Rejection cannot consume the first call's position.
        run.record(request(&expected[0])).unwrap();
        run.record(request(&expected[1])).unwrap();
        assert_rejected_live_and_on_restore(
            &mut run,
            request(&ProviderToolCall {
                id: "third".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }),
            AgentRunError::CanonicalToolCallMismatch,
        );
    }
}

#[test]
fn every_canonical_call_must_be_declared_before_any_result() {
    for failed in [false, true] {
        let mut run = awaiting_response();
        let expected = calls();
        run.record(response_event(ModelResponse::tool_calls(
            "Inspecting".into(),
            expected.clone(),
        )))
        .unwrap();
        run.record(request(&expected[0])).unwrap();
        let result = if failed {
            RunEvent::ToolCallFailed {
                call_id: "first".into(),
                message: "unavailable".into(),
            }
        } else {
            RunEvent::ToolCallSucceeded {
                call_id: "first".into(),
                output: "file contents".into(),
            }
        };
        assert_rejected_live_and_on_restore(
            &mut run,
            result.clone(),
            AgentRunError::CanonicalToolCallMismatch,
        );
        run.record(request(&expected[1])).unwrap();
        // Complete out of order after all declarations; matching is by call ID.
        run.record(RunEvent::ToolCallSucceeded {
            call_id: "second".into(),
            output: "second file".into(),
        })
        .unwrap();
        run.record(result).unwrap();
        assert!(run.can_begin_step());
        let restored = AgentRun::restore(run.id(), run.agent_id(), run.events().to_vec()).unwrap();
        assert!(restored.can_begin_step());
    }
}

fn opaque_state() -> ProviderResponseState {
    ProviderResponseState::new(
        ProviderContextIdentity {
            provider: "custom-provider".into(),
            model: "custom-model".into(),
            format: "custom-v1".into(),
        },
        "custom-format",
        ProviderReplayScope::Response,
        json!({"opaque":"state"}),
        20,
    )
    .unwrap()
}

#[test]
fn historical_context_retains_user_authority_and_replay_state_belongs_to_assistants() {
    for context in [
        MessageContext::Protected,
        MessageContext::Summary {
            checkpoint_id: "checkpoint".into(),
        },
    ] {
        let message = ModelMessage {
            context: Some(context),
            ..ModelMessage::user("Historical evidence".into())
        };
        message.validate().unwrap();
        for role in [
            ModelRole::System,
            ModelRole::Developer,
            ModelRole::Assistant,
            ModelRole::Tool,
        ] {
            assert_eq!(
                ModelMessage {
                    role,
                    ..message.clone()
                }
                .validate(),
                Err(ModelProtocolError::MisplacedContext)
            );
        }
    }
    let assistant = ModelMessage::assistant("Answer".into(), Some(opaque_state()));
    assistant.validate().unwrap();
    for role in [
        ModelRole::System,
        ModelRole::Developer,
        ModelRole::User,
        ModelRole::Tool,
    ] {
        assert_eq!(
            ModelMessage {
                role,
                ..assistant.clone()
            }
            .validate(),
            Err(ModelProtocolError::MisplacedProviderState)
        );
    }
}

#[test]
fn content_blocks_cannot_change_message_roles_or_tool_identity() {
    let reasoning = ContentBlock::Reasoning {
        text: "private".into(),
    };
    let call = ContentBlock::from(calls().remove(0));
    for block in [reasoning, call] {
        assert_eq!(
            ModelMessage {
                content: vec![block],
                ..ModelMessage::user("".into())
            }
            .validate(),
            Err(ModelProtocolError::InvalidBlockRole)
        );
    }
    assert_eq!(
        ModelMessage::assistant_blocks(
            vec![ContentBlock::ToolResult {
                call_id: "first".into(),
                content: "contents".into(),
                is_error: false
            }],
            None
        )
        .validate(),
        Err(ModelProtocolError::InvalidBlockRole)
    );
    assert_eq!(
        ModelMessage {
            role: ModelRole::Tool,
            ..ModelMessage::user("unassociated result".into())
        }
        .validate(),
        Err(ModelProtocolError::InvalidBlockRole)
    );
    assert_eq!(
        ModelMessage::tool_result(" ".into(), "contents".into(), false).validate(),
        Err(ModelProtocolError::InvalidBlockRole)
    );
    for (id, name) in [(" ", "read_file"), ("first", "")] {
        assert_eq!(
            ModelMessage::tool_call(ProviderToolCall {
                id: id.into(),
                name: name.into(),
                arguments: "{}".into()
            })
            .validate(),
            Err(ModelProtocolError::InvalidToolCall)
        );
    }
}

#[test]
fn completion_reasons_cannot_conflict_with_tools_or_terminal_events() {
    for finish_reason in [ModelFinishReason::Stop, ModelFinishReason::InputRequired] {
        let response = ModelResponse {
            finish_reason,
            ..ModelResponse::tool_calls("Inspecting".into(), calls())
        };
        assert_eq!(
            response.validate(),
            Err(ModelProtocolError::ConflictingFinishReason)
        );
        assert_rejected_live_and_on_restore(
            &mut awaiting_response(),
            response_event(response),
            AgentRunError::InvalidCanonicalResponse,
        );
    }
    let mut truncated = awaiting_response();
    truncated
        .record(response_event(ModelResponse {
            finish_reason: ModelFinishReason::Length,
            ..ModelResponse::tool_calls("Inspecting".into(), calls())
        }))
        .unwrap();
    assert_rejected_live_and_on_restore(
        &mut truncated,
        request(&calls()[0]),
        AgentRunError::CanonicalToolCallMismatch,
    );
    for input_required in [false, true] {
        let mut run = awaiting_response();
        let response = if input_required {
            ModelResponse::input_required("Which file?".into())
        } else {
            ModelResponse::final_answer("Done".into())
        };
        run.record(response_event(response)).unwrap();
        for event in [
            RunEvent::RunCompleted {
                answer: "Other answer".into(),
            },
            RunEvent::InputRequested {
                prompt: "Other question".into(),
            },
        ] {
            assert_rejected_live_and_on_restore(
                &mut run,
                event,
                AgentRunError::InvalidCanonicalResponse,
            );
        }
    }
}

#[test]
fn legacy_responses_without_canonical_blocks_still_round_trip_and_restore() {
    let mut run = awaiting_response();
    let legacy: RunEvent = serde_json::from_value(
        json!({"type":"model_response_received","content":"Legacy tool request"}),
    )
    .unwrap();
    assert!(matches!(
        &legacy,
        RunEvent::ModelResponseReceived { response: None, .. }
    ));
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("response")
            .is_none()
    );
    run.record(legacy).unwrap();
    run.record(request(&calls()[0])).unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "first".into(),
        output: "contents".into(),
    })
    .unwrap();
    run.begin_step().unwrap();
    run.record(
        serde_json::from_value(json!({"type":"model_response_received","content":"Done"})).unwrap(),
    )
    .unwrap();
    run.record(RunEvent::RunCompleted {
        answer: "Done".into(),
    })
    .unwrap();
    let restored = AgentRun::restore(run.id(), run.agent_id(), run.events().to_vec()).unwrap();
    assert_eq!(restored.status(), RunStatus::Completed);
    assert_eq!(restored.events(), run.events());
}
