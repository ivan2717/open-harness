use super::*;
use serde_json::json;

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "fixture-provider".to_owned(),
        model: "fixture-model-v1".to_owned(),
        format: "fixture-wire-v1".to_owned(),
    }
}

#[test]
fn unknown_protocols_and_payloads_round_trip_without_core_interpretation() {
    let payload = json!({"custom-state": ["opaque", null, {"signature":"unchanged"}]});
    let state = ProviderResponseState::new(
        identity(),
        "future-vendor-protocol",
        ProviderReplayScope::Prefix,
        payload.clone(),
        37,
    )
    .unwrap();
    assert_eq!(state.version, 1);
    let encoded = serde_json::to_vec(&state).unwrap();
    let decoded: ProviderResponseState = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, state);
    assert_eq!(decoded.payload, payload);
    decoded.validate_envelope().unwrap();
}

#[test]
fn legacy_protocol_strings_without_version_remain_readable() {
    for protocol in [
        "chat_completions",
        "responses",
        "anthropic_messages",
        "gemini_generate_content",
        "third_party_extension",
    ] {
        let persisted = json!({
            "identity": identity(), "protocol": protocol, "scope":"response",
            "payload": {"opaque":"unchanged"}, "replay_tokens": 12,
            "prefix_fingerprint":null
        });
        let state: ProviderResponseState = serde_json::from_value(persisted).unwrap();
        assert_eq!(state.protocol, protocol);
        assert_eq!(state.version, 0);
        state.validate_envelope().unwrap();
    }
}

#[test]
fn envelope_validation_rejects_invalid_identity_and_metadata() {
    let mut state = ProviderResponseState::new(
        identity(),
        "fixture",
        ProviderReplayScope::Response,
        Value::Null,
        0,
    )
    .unwrap();
    state.version = 0;
    state.validate_envelope().unwrap(); // Legacy envelopes remain readable.
    state.version = 2; // Adapter decides whether it supports this version.
    state.validate_envelope().unwrap();
    state.protocol = " ".to_owned();
    assert_eq!(
        state.validate_envelope(),
        Err(ProviderStateError::UnsupportedFormat)
    );
    state.identity.model = " ".to_owned();
    assert_eq!(
        state.validate_envelope(),
        Err(ProviderStateError::InvalidIdentity)
    );
}

#[test]
fn debug_and_errors_do_not_disclose_opaque_payloads() {
    let state = ProviderResponseState::new(
        identity(),
        "fixture",
        ProviderReplayScope::Response,
        json!({"reasoning":"reasoning-secret", "signature":"signature-secret"}),
        0,
    )
    .unwrap();
    let debug = format!("{state:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("reasoning-secret"));
    assert!(!debug.contains("signature-secret"));
}
