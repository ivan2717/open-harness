use super::*;
use crate::WireProtocol as ProviderProtocol;
use agent_core::provider_state::{ProviderContextIdentity, ProviderReplayScope};

fn capture_state(
    identity: ProviderContextIdentity,
    protocol: ProviderProtocol,
    scope: ProviderReplayScope,
    payload: Value,
    replay_tokens: usize,
) -> Result<ProviderResponseState, ProviderStateError> {
    protocol.capture_state(identity, scope, payload, replay_tokens)
}

trait LegacyValidation {
    fn validate_response(
        &self,
        text: &str,
        calls: &[ProviderToolCall],
    ) -> Result<(), ProviderStateError>;
}
impl LegacyValidation for ProviderResponseState {
    fn validate_response(
        &self,
        text: &str,
        calls: &[ProviderToolCall],
    ) -> Result<(), ProviderStateError> {
        validate_legacy_response(self, text, calls)
    }
}
use serde_json::json;

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "fixture-provider".to_owned(),
        model: "fixture-model-v1".to_owned(),
        format: "fixture-wire-v1".to_owned(),
    }
}

fn state(protocol: ProviderProtocol, payload: Value) -> ProviderResponseState {
    capture_state(
        identity(),
        protocol,
        ProviderReplayScope::Response,
        payload,
        37,
    )
    .unwrap()
}

fn call(id: &str, name: &str, arguments: &str) -> ProviderToolCall {
    ProviderToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn assert_round_trip(state: &ProviderResponseState) {
    let encoded = serde_json::to_vec(state).unwrap();
    let decoded: ProviderResponseState = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(&decoded, state);
    assert_eq!(decoded.payload, state.payload);
}

#[test]
fn chat_preserves_complete_assistant_and_ordered_calls() {
    let payload = json!({
        "role": "assistant", "content": "Checking both.",
        "reasoning_content": "private 思考\nexact whitespace ",
        "vendor_extension": { "unknown": [1, null, "opaque"] },
        "tool_calls": [
            {"id":"a", "type":"function", "function":{"name":"lookup", "arguments":"{\"key\":1}"}, "extra_content":{"google":{"thought_signature":"sig-a"}}},
            {"id":"b", "type":"function", "function":{"name":"lookup", "arguments":"{\"key\":2}"}}
        ]
    });
    let state = state(ProviderProtocol::ChatCompletions, payload.clone());
    let calls = [
        call("a", "lookup", r#"{ "key": 1 }"#),
        call("b", "lookup", r#"{"key":2}"#),
    ];
    state.validate_response("Checking both.", &calls).unwrap();
    assert_eq!(state.payload, payload);
    assert_round_trip(&state);
    assert_eq!(
        state.validate_response("Checking both.", &[calls[1].clone(), calls[0].clone()]),
        Err(ProviderStateError::ToolCallsMismatch)
    );
    assert_eq!(
        state.validate_response("Changed", &calls),
        Err(ProviderStateError::ContentMismatch)
    );
    assert_eq!(
        state.validate_response("Checking both.", &calls[..1]),
        Err(ProviderStateError::ToolCallsMismatch)
    );
}

#[test]
fn responses_preserves_entire_ordered_output_including_opaque_items() {
    let payload = json!([
        {"type":"reasoning", "id":"rs_1", "summary":[], "encrypted_content":"encrypted-secret"},
        {"type":"function_call", "id":"fc_item_1", "call_id":"call_1", "name":"lookup", "arguments":"{\"b\":2,\"a\":1}", "status":"completed"},
        {"type":"reasoning", "id":"rs_2", "summary":[{"type":"summary_text", "text":"summary-secret"}]},
        {"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"Done", "annotations":[]}, {"type":"output_text", "text":"."}]},
        {"type":"future_metadata", "extra":{"unchanged":true}}
    ]);
    let state = state(ProviderProtocol::Responses, payload.clone());
    state
        .validate_response("Done.", &[call("call_1", "lookup", r#"{"a":1,"b":2}"#)])
        .unwrap();
    assert_eq!(state.payload, payload);
    assert_round_trip(&state);
    assert_eq!(
        state.validate_response("Done.", &[call("fc_item_1", "lookup", r#"{"a":1,"b":2}"#)]),
        Err(ProviderStateError::ToolCallsMismatch)
    );
}

#[test]
fn anthropic_preserves_redacted_empty_and_interleaved_thinking_blocks() {
    let payload = json!({
        "id":"msg_1", "role":"assistant", "type":"message", "content":[
            {"type":"thinking", "thinking":"", "signature":"opaque-signature"},
            {"type":"redacted_thinking", "data":"opaque-redacted"},
            {"type":"text", "text":"First"},
            {"type":"tool_use", "id":"call_a", "name":"lookup", "input":{"a":1}},
            {"type":"thinking", "thinking":"private-thinking", "signature":"signature-2"},
            {"type":"tool_use", "id":"call_b", "name":"lookup", "input":{"a":2}},
            {"type":"text", "text":" second"}
        ], "stop_reason":"tool_use"
    });
    let mut state = capture_state(
        identity(),
        ProviderProtocol::AnthropicMessages,
        ProviderReplayScope::Prefix,
        payload.clone(),
        91,
    )
    .unwrap();
    assert_eq!(state.prefix_fingerprint, None);
    state.prefix_fingerprint = Some("runtime-bound-prefix".to_owned());
    let calls = [
        call("call_a", "lookup", r#"{"a":1}"#),
        call("call_b", "lookup", r#"{"a":2}"#),
    ];
    state.validate_response("First second", &calls).unwrap();
    assert_eq!(state.payload, payload);
    assert_round_trip(&state);
    assert_eq!(
        state.validate_response("First second", &[calls[1].clone(), calls[0].clone()]),
        Err(ProviderStateError::ToolCallsMismatch)
    );
}

#[test]
fn gemini_keeps_part_signatures_and_binds_absent_ids_by_ordinal() {
    let payload = json!({"role":"model", "parts":[
        {"thought":true, "text":"private summary"},
        {"functionCall":{"name":"lookup", "args":{"key":1}}, "thoughtSignature":"first-call-signature"},
        {"functionCall":{"id":"wire_b", "name":"lookup", "args":{"key":2}}},
        {"text":"Done"},
        {"text":"", "thoughtSignature":"empty-text-signature", "vendor":true}
    ]});
    let state = state(ProviderProtocol::GeminiGenerateContent, payload.clone());
    let calls = [
        call("generated_a", "lookup", r#"{"key":1}"#),
        call("wire_b", "lookup", r#"{"key":2}"#),
    ];
    state.validate_response("Done", &calls).unwrap();
    assert_eq!(state.payload, payload);
    assert_round_trip(&state);
    assert_eq!(
        state.validate_response("Done", &[calls[1].clone(), calls[0].clone()]),
        Err(ProviderStateError::ToolCallsMismatch)
    );
    assert_eq!(
        state.validate_response(
            "Done",
            &[calls[0].clone(), call("other_id", "lookup", r#"{"key":2}"#)]
        ),
        Err(ProviderStateError::ToolCallsMismatch)
    );
}

#[test]
fn all_protocols_accept_ordinary_responses_without_reasoning() {
    let fixtures = [
        (
            ProviderProtocol::ChatCompletions,
            json!({"role":"assistant", "content":"Hello"}),
        ),
        (
            ProviderProtocol::Responses,
            json!([{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"Hello"}]}]),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"role":"assistant", "content":[{"type":"text", "text":"Hello"}]}),
        ),
        (
            ProviderProtocol::GeminiGenerateContent,
            json!({"role":"model", "parts":[{"text":"Hello"}]}),
        ),
    ];
    for (protocol, payload) in fixtures {
        state(protocol, payload)
            .validate_response("Hello", &[])
            .unwrap();
    }
}

#[test]
fn refusal_text_is_bound_as_visible_content() {
    let fixtures = [
        (
            ProviderProtocol::ChatCompletions,
            json!({"role":"assistant", "content":null, "refusal":"Cannot help."}),
        ),
        (
            ProviderProtocol::Responses,
            json!([{"type":"message", "role":"assistant", "content":[{"type":"refusal", "refusal":"Cannot help."}]}]),
        ),
    ];
    for (protocol, payload) in fixtures {
        let state = state(protocol, payload);
        state.validate_response("Cannot help.", &[]).unwrap();
        assert_eq!(
            state.validate_response("", &[]),
            Err(ProviderStateError::ContentMismatch)
        );
    }
}

#[test]
fn payload_roles_shapes_and_unsupported_calls_are_rejected() {
    let fixtures = [
        (
            ProviderProtocol::ChatCompletions,
            json!({"role":"user", "content":"secret"}),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::ChatCompletions,
            json!({"role":"assistant", "tool_calls":"secret"}),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::ChatCompletions,
            json!({"role":"assistant", "function_call":{"name":"legacy"}}),
            ProviderStateError::UnsupportedToolCall,
        ),
        (
            ProviderProtocol::Responses,
            json!([{"type":"message", "role":"system", "content":[]}]),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::Responses,
            json!([{"type":"reasoning", "role":"user"}]),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::Responses,
            json!([{"type":"web_search_call", "id":"secret"}]),
            ProviderStateError::UnsupportedToolCall,
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"role":"assistant", "content":[{"type":"thinking", "thinking":"secret"}]}),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"role":"assistant", "content":[{"type":"server_tool_use", "name":"secret"}]}),
            ProviderStateError::UnsupportedToolCall,
        ),
        (
            ProviderProtocol::GeminiGenerateContent,
            json!({"role":"user", "parts":[]}),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::GeminiGenerateContent,
            json!({"role":"model", "parts":[{"thoughtSignature":42}]}),
            ProviderStateError::InvalidPayload,
        ),
        (
            ProviderProtocol::GeminiGenerateContent,
            json!({"role":"model", "parts":[{"functionResponse":{"name":"secret"}}]}),
            ProviderStateError::UnsupportedToolCall,
        ),
    ];
    for (protocol, payload, expected) in fixtures {
        assert_eq!(
            capture_state(
                identity(),
                protocol,
                ProviderReplayScope::Response,
                payload,
                0
            )
            .unwrap_err(),
            expected
        );
    }
}

#[test]
fn validation_checks_changed_names_arguments_and_public_fields() {
    let mut state = state(
        ProviderProtocol::ChatCompletions,
        json!({
            "role":"assistant", "content":null, "tool_calls":[
                {"id":"a", "type":"function", "function":{"name":"lookup", "arguments":"{\"key\":1}"}}
            ]
        }),
    );
    for call in [
        call("a", "different", r#"{"key":1}"#),
        call("a", "lookup", r#"{"key":2}"#),
    ] {
        assert_eq!(
            state.validate_response("", &[call]),
            Err(ProviderStateError::ToolCallsMismatch)
        );
    }
    state.payload["role"] = json!("user");
    assert_eq!(
        state.validate_response("", &[]),
        Err(ProviderStateError::InvalidPayload)
    );
    state.identity.model = "  ".to_owned();
    assert_eq!(
        state.validate_response("", &[]),
        Err(ProviderStateError::InvalidIdentity)
    );
}

#[test]
fn debug_and_errors_do_not_disclose_payloads() {
    let state = state(
        ProviderProtocol::ChatCompletions,
        json!({
            "role":"assistant", "content":"visible-secret", "reasoning_content":"reasoning-secret"
        }),
    );
    let debug = format!("{state:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("visible-secret"));
    assert!(!debug.contains("reasoning-secret"));
    let error = state.validate_response("wrong", &[]).unwrap_err();
    assert!(!format!("{error}: {error:?}").contains("secret"));
}

#[test]
fn normalized_content_preserves_interleaved_tools_text_and_reasoning() {
    let payload = json!({"role":"assistant", "content":[
        {"type":"thinking","thinking":"first reasoning","signature":"s1"},
        {"type":"text","text":"before"},
        {"type":"tool_use","id":"a","name":"lookup","input":{"key":1}},
        {"type":"text","text":"after"},
        {"type":"thinking","thinking":"second reasoning","signature":"s2"},
        {"type":"tool_use","id":"b","name":"lookup","input":{"key":2}}
    ]});
    let state = state(ProviderProtocol::AnthropicMessages, payload.clone());
    let content = ProviderProtocol::AnthropicMessages
        .decode_content(&payload, "response-1")
        .unwrap();
    assert!(matches!(&content[0], ContentBlock::Reasoning { text } if text == "first reasoning"));
    assert!(matches!(&content[2], ContentBlock::ToolCall { id, .. } if id == "a"));
    assert!(matches!(&content[3], ContentBlock::Text { text } if text == "after"));
    validate_replay_state(&state, &content).unwrap();
    let mut reordered = content.clone();
    reordered.swap(2, 3);
    assert_eq!(
        validate_replay_state(&state, &reordered),
        Err(ProviderStateError::ContentMismatch)
    );
    let mut modified_reasoning = content.clone();
    modified_reasoning[0] = ContentBlock::Reasoning {
        text: "changed".to_owned(),
    };
    assert_eq!(
        validate_replay_state(&state, &modified_reasoning),
        Err(ProviderStateError::ContentMismatch)
    );
    let without_reasoning: Vec<_> = content
        .into_iter()
        .filter(|block| !matches!(block, ContentBlock::Reasoning { .. }))
        .collect();
    validate_replay_state(&state, &without_reasoning).unwrap();
}

#[test]
fn only_legacy_envelopes_accept_flattened_interleaving() {
    let payload = json!([
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"first"}]},
        {"type":"function_call","call_id":"a","name":"lookup","arguments":"{}"},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"second"}]}
    ]);
    let mut state = state(ProviderProtocol::Responses, payload);
    let flattened = vec![
        ContentBlock::Text {
            text: "firstsecond".to_owned(),
        },
        ContentBlock::ToolCall {
            id: "a".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
        },
    ];
    assert_eq!(
        validate_replay_state(&state, &flattened),
        Err(ProviderStateError::ContentMismatch)
    );
    state.version = 0;
    validate_replay_state(&state, &flattened).unwrap();
    state.version = 2;
    assert_eq!(
        validate_replay_state(&state, &flattened),
        Err(ProviderStateError::InvalidVersion)
    );
}

#[test]
fn all_four_codecs_decode_and_validate_their_own_normalized_responses() {
    let fixtures = [
        (
            ProviderProtocol::ChatCompletions,
            json!({
                "role":"assistant", "content":"answer", "reasoning_content":"thought",
                "tool_calls":[{"id":"a","type":"function","function":{"name":"lookup","arguments":"{}"}}]
            }),
        ),
        (
            ProviderProtocol::Responses,
            json!([
                {"type":"reasoning","id":"rs","summary":[],"content":[{"type":"reasoning_text","text":"thought"}],"encrypted_content":"opaque"},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]},
                {"type":"function_call","call_id":"a","name":"lookup","arguments":"{}"}
            ]),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"role":"assistant","content":[
                {"type":"thinking","thinking":"thought","signature":"sig"},
                {"type":"text","text":"answer"},
                {"type":"tool_use","id":"a","name":"lookup","input":{}}
            ]}),
        ),
        (
            ProviderProtocol::GeminiGenerateContent,
            json!({"role":"model","parts":[
                {"text":"thought","thought":true,"thoughtSignature":"sig"},
                {"text":"answer"}, {"functionCall":{"id":"a","name":"lookup","args":{}}}
            ]}),
        ),
    ];
    for (protocol, payload) in fixtures {
        let state = state(protocol, payload.clone());
        let content = protocol.decode_content(&payload, "response-1").unwrap();
        assert_eq!(content.len(), 3);
        assert!(matches!(&content[0], ContentBlock::Reasoning { text } if text == "thought"));
        validate_replay_state(&state, &content).unwrap();
    }
}

#[test]
fn generated_call_ids_are_stable_and_do_not_collide_with_wire_ids() {
    let payload = json!({"role":"model","parts":[
        {"functionCall":{"name":"lookup","args":{}}},
        {"functionCall":{"id":"response-1:call:1","name":"lookup","args":{}}},
        {"functionCall":{"name":"lookup","args":{}}}
    ]});
    let protocol = ProviderProtocol::GeminiGenerateContent;
    let content = protocol.decode_content(&payload, "response-1").unwrap();
    assert_eq!(
        content,
        protocol.decode_content(&payload, "response-1").unwrap()
    );
    let ids: Vec<_> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        [
            "response-1:call:2",
            "response-1:call:1",
            "response-1:call:3"
        ]
    );
    validate_replay_state(&state(protocol, payload), &content).unwrap();
}

#[test]
fn adapter_rejects_unknown_formats_without_restricting_core_envelopes() {
    let custom = ProviderResponseState::new(
        identity(),
        "custom-vendor",
        ProviderReplayScope::Response,
        json!({"result":"hello"}),
        0,
    )
    .unwrap();
    custom.validate_envelope().unwrap();
    assert_eq!(
        validate_replay_state(
            &custom,
            &[ContentBlock::Text {
                text: "hello".to_owned()
            }]
        ),
        Err(ProviderStateError::UnsupportedFormat)
    );
}

#[test]
fn missing_wire_call_ids_use_distinct_namespaces_across_model_rounds() {
    let payload = json!({"role":"model","parts":[
        {"functionCall":{"name":"lookup","args":{"key":1}},"thoughtSignature":"opaque"}
    ]});
    let protocol = ProviderProtocol::GeminiGenerateContent;
    let first = protocol.decode_content(&payload, "run-7-step-1").unwrap();
    let second = protocol.decode_content(&payload, "run-7-step-2").unwrap();
    let ContentBlock::ToolCall { id: first_id, .. } = &first[0] else {
        panic!("expected call");
    };
    let ContentBlock::ToolCall { id: second_id, .. } = &second[0] else {
        panic!("expected call");
    };
    assert_eq!(first_id, "run-7-step-1:call:1");
    assert_eq!(second_id, "run-7-step-2:call:1");
    assert_ne!(first_id, second_id);
    let state = state(protocol, payload.clone());
    validate_replay_state(&state, &first).unwrap();
    validate_replay_state(&state, &second).unwrap();
    assert_eq!(
        first,
        protocol.decode_content(&payload, "run-7-step-1").unwrap()
    );
    assert_eq!(
        protocol.decode_content(&payload, " "),
        Err(ProviderStateError::InvalidIdentity)
    );
}
