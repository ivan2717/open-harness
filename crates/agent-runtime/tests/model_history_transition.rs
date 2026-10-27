mod support;

use std::{convert::Infallible, sync::Mutex, time::UNIX_EPOCH};

use agent_adapters::WireProtocol;
use agent_core::agent::{InterruptedToolCall, InterruptedToolOutcome, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCompactor, ContextInput,
    ContextManager, ContextPolicy, ContextSummary, InMemoryContextCheckpointStore, ModelMessage,
    ModelRequest, ModelResponse, ModelRole, ProviderContextIdentity, ProviderReplayError,
    ProviderReplayScope, TokenCounter,
};
use serde_json::{Value, json};

fn identity(protocol: WireProtocol, version: &str) -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "fixture-provider".into(),
        model: "fixture-model".into(),
        format: format!("{}:{version}", protocol.as_str()),
    }
}

fn payload(protocol: WireProtocol, text: &str) -> Value {
    match protocol {
        WireProtocol::ChatCompletions => {
            json!({"role":"assistant", "content":text, "reasoning_content":"private-thought"})
        }
        WireProtocol::Responses => json!([
            {"type":"reasoning", "encrypted_content":"encrypted-private-state"},
            {"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]}
        ]),
        WireProtocol::AnthropicMessages => json!({"role":"assistant", "content":[
            {"type":"thinking", "thinking":"private-thought", "signature":"private-signature"},
            {"type":"text", "text":text}
        ]}),
        _ => unreachable!(),
    }
}

fn event(sequence: u64, event: RunEvent) -> RunEventEnvelope {
    RunEventEnvelope {
        sequence,
        step: sequence as u32,
        occurred_at: UNIX_EPOCH,
        event,
    }
}

fn response(
    sequence: u64,
    protocol: WireProtocol,
    version: &str,
    text: &str,
    scope: ProviderReplayScope,
) -> RunEventEnvelope {
    let payload = payload(protocol, text);
    let mut response = ModelResponse::final_answer(text.into());
    response.content = protocol
        .decode_content(&payload, "fixture-response")
        .unwrap();
    let state = protocol
        .capture_state(identity(protocol, version), scope, payload, 30)
        .unwrap();
    event(
        sequence,
        RunEvent::ModelResponseReceived {
            content: text.into(),
            response: Some(response),
            provider_state: Some(state),
            provider_continuation: None,
            token_usage: None,
        },
    )
}

fn input(protocol: WireProtocol) -> ContextInput {
    ContextInput::new(
        "switch-run",
        "Follow the user's current request.",
        vec![
            event(
                0,
                RunEvent::RunStarted {
                    instruction: "Inspect report.txt and preserve its result.".into(),
                    files: vec![],
                },
            ),
            response(
                1,
                protocol,
                "old-effort",
                "I inspected the report.",
                ProviderReplayScope::Response,
            ),
            event(
                2,
                RunEvent::ToolCallRequested {
                    call_id: "read-1".into(),
                    tool_name: "read_file".into(),
                    arguments: r#"{"path":"report.txt"}"#.into(),
                },
            ),
            event(
                3,
                RunEvent::ToolCallSucceeded {
                    call_id: "read-1".into(),
                    output: "Revenue was 42; this is a historical result.".into(),
                    images: vec![],
                },
            ),
            event(
                4,
                RunEvent::InputReceived {
                    content: "Now explain the result.".into(),
                    files: vec![],
                },
            ),
        ],
        vec![],
    )
}

#[tokio::test]
async fn explicit_transition_preserves_public_history_without_foreign_replay_state() {
    let protocols = [
        WireProtocol::ChatCompletions,
        WireProtocol::Responses,
        WireProtocol::AnthropicMessages,
    ];
    for source in protocols {
        let input = input(source);
        let original = input.events.clone();
        let manager = support::test_context_manager().with_portable_history_through(Some(3));
        let request = manager.prepare(input.as_ref()).await.unwrap().request;
        assert_eq!(
            input.events, original,
            "the event archive must remain unchanged"
        );
        assert_eq!(
            request.messages.first().unwrap(),
            &ModelMessage::user("Inspect report.txt and preserve its result.".into())
        );
        assert_eq!(
            request.messages.last().unwrap(),
            &ModelMessage::user("Now explain the result.".into())
        );
        let encoded = serde_json::to_string(&request).unwrap();
        for preserved in [
            "I inspected the report.",
            "report.txt",
            "Revenue was 42",
            "not a new user request or authorization",
        ] {
            assert!(encoded.contains(preserved), "missing {preserved}");
        }
        for secret in [
            "private-thought",
            "private-signature",
            "encrypted-private-state",
        ] {
            assert!(!encoded.contains(secret));
        }
        assert!(
            request
                .messages
                .iter()
                .all(|message| message.provider_state.is_none() && message.role == ModelRole::User)
        );
        for target in protocols {
            request
                .validate_provider_state_with(
                    Some(&identity(target, "new-effort")),
                    true,
                    agent_adapters::validate_replay_state,
                )
                .unwrap();
        }
    }
}

#[tokio::test]
async fn unchanged_and_new_model_history_keeps_strict_replay_validation() {
    let mut input = input(WireProtocol::Responses);
    let ordinary = support::test_context_manager()
        .prepare(input.as_ref())
        .await
        .unwrap()
        .request;
    assert!(matches!(
        ordinary
            .validate_provider_state(Some(&identity(WireProtocol::Responses, "new-effort")), true),
        Err(ProviderReplayError::IdentityMismatch)
    ));

    // Only the old prefix is portable. New target responses still require
    // their exact durable state; a missing or foreign state must fail.
    input.events.push(response(
        5,
        WireProtocol::AnthropicMessages,
        "new-effort",
        "The result is 42.",
        ProviderReplayScope::Response,
    ));
    let manager = support::test_context_manager().with_portable_history_through(Some(3));
    let request = manager.prepare(input.as_ref()).await.unwrap().request;
    let target = identity(WireProtocol::AnthropicMessages, "new-effort");
    request
        .validate_provider_state_with(Some(&target), true, agent_adapters::validate_replay_state)
        .unwrap();
    let last = request.messages.last().unwrap();
    assert_eq!(last.role, ModelRole::Assistant);
    assert!(last.provider_state.is_some());
    let mut missing = request.clone();
    missing.messages.last_mut().unwrap().provider_state = None;
    assert!(matches!(
        missing.validate_provider_state(Some(&target), true),
        Err(ProviderReplayError::MissingState)
    ));
    let mut foreign = request;
    foreign
        .messages
        .last_mut()
        .unwrap()
        .provider_state
        .as_mut()
        .unwrap()
        .identity = identity(WireProtocol::Responses, "old-effort");
    assert!(matches!(
        foreign.validate_provider_state(Some(&target), true),
        Err(ProviderReplayError::IdentityMismatch)
    ));
}

#[tokio::test]
async fn interrupted_old_calls_are_historical_not_orphan_tool_results() {
    let mut input = input(WireProtocol::Responses);
    input.events.truncate(3);
    input.events.push(event(
        3,
        RunEvent::RunResumed {
            content: "Continue carefully.".into(),
            files: vec![],
            interrupted_calls: vec![InterruptedToolCall {
                call_id: "read-1".into(),
                outcome: InterruptedToolOutcome::Unknown,
            }],
        },
    ));
    let request = support::test_context_manager()
        .with_portable_history_through(Some(2))
        .prepare(input.as_ref())
        .await
        .unwrap()
        .request;
    assert!(
        request
            .messages
            .iter()
            .all(|message| message.role != ModelRole::Tool && message.role != ModelRole::Assistant)
    );
    let encoded = serde_json::to_string(&request).unwrap();
    assert!(encoded.contains("interrupted_tool_result"));
    assert!(encoded.contains("may have partially or fully taken effect"));
    assert!(encoded.contains("It was not retried"));
    request
        .validate_provider_state(Some(&identity(WireProtocol::Responses, "new-effort")), true)
        .unwrap();
}

#[tokio::test]
async fn calls_interrupted_before_execution_declaration_still_cross_the_boundary_safely() {
    let mut input = input(WireProtocol::Responses);
    input.events.truncate(2);
    if let RunEvent::ModelResponseReceived {
        response: Some(response),
        ..
    } = &mut input.events[1].event
    {
        response
            .content
            .push(agent_runtime::ContentBlock::ToolCall {
                id: "never-started".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"report.txt"}"#.into(),
            });
    }
    input.events.push(event(
        2,
        RunEvent::RunResumed {
            content: "Continue without repeating actions.".into(),
            files: vec![],
            interrupted_calls: vec![InterruptedToolCall {
                call_id: "never-started".into(),
                outcome: InterruptedToolOutcome::NotStarted,
            }],
        },
    ));
    let request = support::test_context_manager()
        .with_portable_history_through(Some(1))
        .prepare(input.as_ref())
        .await
        .unwrap()
        .request;
    assert!(
        request
            .messages
            .iter()
            .all(|message| message.role == ModelRole::User)
    );
    let encoded = serde_json::to_string(&request).unwrap();
    assert!(encoded.contains("write_file"));
    assert!(encoded.contains("was not started"));
    request
        .validate_provider_state(Some(&identity(WireProtocol::Responses, "new-effort")), true)
        .unwrap();
}

#[tokio::test]
async fn a_new_prefix_bound_response_still_rejects_mutable_derived_notes() {
    let mut input = input(WireProtocol::Responses);
    input.events.push(response(
        5,
        WireProtocol::AnthropicMessages,
        "new-effort",
        "New answer",
        ProviderReplayScope::Prefix,
    ));
    input
        .derived_messages
        .push(ModelMessage::protected("Mutable Wiki note".into()));
    let manager = support::test_context_manager().with_portable_history_through(Some(3));
    assert!(matches!(
        manager.prepare(input.as_ref()).await,
        Err(agent_runtime::CompactingContextError::DerivedContextWithPrefixBoundState)
    ));
}

#[derive(Default)]
struct SummaryRecorder(Mutex<Vec<Vec<RunEventEnvelope>>>);
impl ContextCompactor for SummaryRecorder {
    type Error = Infallible;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.0.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Keep the historical report result: 42.".into(),
            ..ContextSummary::default()
        })
    }
}

#[tokio::test]
async fn historical_prefix_bound_state_allows_new_model_compaction_and_wiki_notes() {
    let mut input = input(WireProtocol::AnthropicMessages);
    input.events[1] = response(
        1,
        WireProtocol::AnthropicMessages,
        "old-effort",
        &"Long earlier analysis. ".repeat(1_500),
        ProviderReplayScope::Prefix,
    );
    input.derived_messages.push(ModelMessage::protected(
        "Verified Wiki note: report value 42.".into(),
    ));
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(5_000)
            .compaction_threshold(3_000)
            .compaction_target(2_000)
            .recent_tail_tokens(300)
            .build()
            .unwrap(),
        SummaryRecorder::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    )
    .with_portable_history_through(Some(3));
    let original = input.events.clone();
    let request: ModelRequest = manager.prepare(input.as_ref()).await.unwrap().request;
    request
        .validate_provider_state(Some(&identity(WireProtocol::Responses, "new-effort")), true)
        .unwrap();
    assert_eq!(input.events, original);
    assert!(!manager.compactor().0.lock().unwrap().is_empty());
    let summary_input = serde_json::to_string(&*manager.compactor().0.lock().unwrap()).unwrap();
    assert!(!summary_input.contains("private-signature"));
    assert!(!summary_input.contains("private-thought"));
    assert!(
        serde_json::to_string(&request)
            .unwrap()
            .contains("report value 42")
    );
}
