use std::convert::Infallible;

use agent_core::agent::RunEvent;
use agent_runtime::{
    ContentBlock, Model, ModelMessage, ModelRequest, ModelResponse, ProviderContextIdentity,
    ProviderReplayError, ProviderReplayScope, ProviderResponseState, ProviderStateError,
};
use serde_json::json;

// Captured pre-refactor serialization, including the old separate call message.
// Keep this literal and binding independent of the new production serializer.
const LEGACY_PREFIX: &str = r#"["system",[],null,[{"User":{"content":"inspect"}},{"ProtectedContext":{"content":"historical"}},{"ContextSummary":{"checkpoint_id":"checkpoint-1","content":"summary"}},{"Assistant":{"content":"before","provider_state":{"identity":{"provider":"custom","model":"model","format":"custom/v1"},"protocol":"custom_protocol","scope":"response","payload":{"body":"before","call":"read-1"},"replay_tokens":3,"prefix_fingerprint":null}}},{"ToolCall":{"call_id":"read-1","tool_name":"lookup","arguments":"{}"}},{"ToolResult":{"call_id":"read-1","output":"value"}},{"User":{"content":"continue"}}]]"#;
const LEGACY_BINDING: &str = "v1:685baa0691cf2203d1aa9a1d03d20f8c";

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "custom".into(),
        model: "model".into(),
        format: "custom/v1".into(),
    }
}

fn persisted_state(
    body: &str,
    scope: &str,
    binding: Option<&str>,
    call: Option<&str>,
) -> ProviderResponseState {
    let mut payload = json!({"body":body});
    if let Some(call) = call {
        payload["call"] = json!(call);
    }
    let event: RunEvent = serde_json::from_value(json!({
        "type":"model_response_received", "content":body,
        "provider_state":{
            "identity":{"provider":"custom","model":"model","format":"custom/v1"},
            "protocol":"custom_protocol", "scope":scope, "payload":payload,
            "replay_tokens":3, "prefix_fingerprint":binding
        }
    }))
    .unwrap();
    let RunEvent::ModelResponseReceived {
        provider_state: Some(state),
        ..
    } = event
    else {
        panic!("expected response state");
    };
    assert_eq!(state.version, 0);
    state
}

fn legacy_request() -> ModelRequest {
    ModelRequest {
        instructions: "system".into(),
        tools: vec![],
        max_output_tokens: None,
        provider_context: None,
        messages: vec![
            ModelMessage::user("inspect".into()),
            ModelMessage::protected("historical".into()),
            ModelMessage::summary("checkpoint-1".into(), "summary".into()),
            ModelMessage::assistant_blocks(
                vec![
                    ContentBlock::Text {
                        text: "before".into(),
                    },
                    ContentBlock::ToolCall {
                        id: "read-1".into(),
                        name: "lookup".into(),
                        arguments: "{}".into(),
                    },
                ],
                Some(persisted_state("before", "response", None, Some("read-1"))),
            ),
            ModelMessage::tool_result("read-1".into(), "value".into(), false),
            ModelMessage::user("continue".into()),
            ModelMessage::assistant(
                "done".into(),
                Some(persisted_state(
                    "done",
                    "prefix",
                    Some(LEGACY_BINDING),
                    None,
                )),
            ),
        ],
    }
}

// This format does not exist in agent-adapters. The harness needs only this
// adapter-owned hook, not another core protocol enum variant or parser branch.
struct CustomAdapter {
    identity: ProviderContextIdentity,
}

impl Model for CustomAdapter {
    type Error = Infallible;

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    fn requires_provider_state(&self) -> bool {
        true
    }

    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        if state.protocol != "custom_protocol" {
            return Err(ProviderStateError::UnsupportedFormat);
        }
        if state.version > 1 {
            return Err(ProviderStateError::InvalidVersion);
        }
        let text: String = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if state.payload["body"].as_str() != Some(text.as_str()) {
            return Err(ProviderStateError::ContentMismatch);
        }
        let calls: Vec<_> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((id.as_str(), name.as_str(), arguments.as_str())),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = state.payload["call"]
            .as_str()
            .map(|id| (id, "lookup", "{}"))
            .into_iter()
            .collect();
        if calls != expected {
            return Err(ProviderStateError::ToolCallsMismatch);
        }
        Ok(())
    }

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(ModelResponse::final_answer("done".into()))
    }
}

fn adapter() -> CustomAdapter {
    CustomAdapter {
        identity: identity(),
    }
}

#[test]
fn historical_json_events_keep_their_old_prefix_binding_after_projection() {
    // The old serializer's golden vector is independent from the runtime code.
    let mut a = 0xcbf2_9ce4_8422_2325_u64;
    let mut b = 0x8422_2325_cbf2_9ce4_u64;
    for byte in b"provider-replay-prefix-v1"
        .iter()
        .chain(LEGACY_PREFIX.as_bytes())
    {
        a = (a ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        b = (b ^ u64::from(*byte).rotate_left(1)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(format!("v1:{a:016x}{b:016x}"), LEGACY_BINDING);
    let request = legacy_request();
    adapter().validate_request(&request).unwrap();
    let state = request.messages[3].provider_state.as_ref().unwrap();
    assert!(
        serde_json::to_value(state)
            .unwrap()
            .get("version")
            .is_none()
    );
}

#[test]
fn historical_prefix_fallback_does_not_accept_changed_or_new_semantics() {
    let mut changed_instruction = legacy_request();
    changed_instruction.instructions.push('!');
    let mut changed_user = legacy_request();
    changed_user.messages[0] = ModelMessage::user("different".into());
    let mut changed_result = legacy_request();
    changed_result.messages[4] =
        ModelMessage::tool_result("read-1".into(), "different".into(), false);
    let mut added_reasoning = legacy_request();
    added_reasoning.messages[0]
        .content
        .push(ContentBlock::Reasoning {
            text: "new content".into(),
        });
    let mut added_new_state = legacy_request();
    added_new_state.messages[3]
        .provider_state
        .as_mut()
        .unwrap()
        .version = 1;
    let mut new_bound_state = legacy_request();
    new_bound_state.messages[6]
        .provider_state
        .as_mut()
        .unwrap()
        .version = 1;
    let mut reordered_content = legacy_request();
    reordered_content.messages[3].content.swap(0, 1);
    let mut changed_authority = legacy_request();
    changed_authority.messages[1].context = None;
    for request in [
        changed_instruction,
        changed_user,
        changed_result,
        added_reasoning,
        added_new_state,
        new_bound_state,
        reordered_content,
        changed_authority,
    ] {
        assert!(matches!(
            adapter().validate_request(&request),
            Err(ProviderReplayError::PrefixChanged)
        ));
    }
}

#[test]
fn custom_protocol_replays_through_its_adapter_without_core_protocol_registration() {
    let state = ProviderResponseState::new(
        identity(),
        "custom_protocol",
        ProviderReplayScope::Response,
        json!({"body":"custom answer"}),
        0,
    )
    .unwrap();
    let request = ModelRequest {
        instructions: "system".into(),
        messages: vec![ModelMessage::assistant("custom answer".into(), Some(state))],
        tools: vec![],
        max_output_tokens: None,
        provider_context: None,
    };
    adapter().validate_request(&request).unwrap();
    let mut changed = request;
    changed.messages[0].content = vec![ContentBlock::Text {
        text: "modified answer".into(),
    }];
    assert!(matches!(
        adapter().validate_request(&changed),
        Err(ProviderReplayError::InvalidResponse(
            ProviderStateError::ContentMismatch
        ))
    ));
}
