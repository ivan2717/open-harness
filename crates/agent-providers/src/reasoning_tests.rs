use super::*;
use agent_runtime::ModelMessage;
use serde_json::json;

fn model(protocol: ProviderProtocol) -> HttpModel {
    HttpModel::new(ProviderConfig {
        protocol,
        base_url: "https://relay.example/v1".into(),
        api_key: "fixture".into(),
        model: "gpt-6-luna".into(),
        context_window: 32000,
        max_output_tokens: 8192,
        reasoning: ReasoningMode::Off,
        supports_images: false,
    })
    .unwrap()
}
fn request() -> ModelRequest {
    ModelRequest {
        instructions: "Answer".into(),
        messages: vec![ModelMessage::user("Hello".into())],
        tools: vec![],
        max_output_tokens: None,
        provider_context: None,
    }
}
fn capability(adapter: &str, options: Value) -> ReasoningCapability {
    serde_json::from_value(json!({"source":"manual","adapter":adapter,"options":options})).unwrap()
}

#[test]
fn protocol_presets_offer_candidates_but_leave_default_requests_unchanged() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        let model = model(protocol);
        assert_eq!(model.reasoning_capability().source, ReasoningSource::Preset);
        let expected = if protocol == ProviderProtocol::AnthropicMessages {
            vec!["off", "budget_1024", "budget_4096"]
        } else {
            vec!["low", "medium", "high"]
        };
        assert_eq!(model.supported_reasoning_efforts(), expected);
        assert!(model.with_reasoning_effort("ultra").is_err());
        let body = model.encode(&request()).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }
}

#[test]
fn declared_choices_select_each_provider_dialect_and_allow_custom_wire_values() {
    for (protocol, adapter, expected) in [
        (
            ProviderProtocol::ChatCompletions,
            "effort",
            json!({"reasoning_effort":"xhigh"}),
        ),
        (
            ProviderProtocol::Responses,
            "effort",
            json!({"reasoning":{"effort":"xhigh"}}),
        ),
        (
            ProviderProtocol::ChatCompletions,
            "deepseek",
            json!({"thinking":{"type":"enabled"},"reasoning_effort":"xhigh"}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            "messages_effort",
            json!({"thinking":{"type":"enabled"},"output_config":{"effort":"xhigh"}}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            "messages_effort_only",
            json!({"output_config":{"effort":"xhigh"}}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            "anthropic_adaptive",
            json!({"thinking":{"type":"adaptive"},"output_config":{"effort":"xhigh"}}),
        ),
    ] {
        let base = model(protocol)
            .with_reasoning_capability(capability(
                adapter,
                json!([{"id":"maximum","label":"Maximum","effort":"xhigh"}]),
            ))
            .unwrap();
        assert_eq!(base.supported_reasoning_efforts(), vec!["maximum"]);
        assert!(
            base.with_reasoning_effort("xhigh").is_err(),
            "wire values are not UI IDs"
        );
        let selected = base.with_reasoning_effort("maximum").unwrap();
        let body = selected.encode(&request()).unwrap();
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&body[key], value, "{adapter}: {key}");
        }
        if adapter == "messages_effort_only" {
            assert!(body.get("thinking").is_none());
        }
        assert_eq!(
            base.reasoning,
            ReasoningMode::Off,
            "default remains unmodified"
        );
    }
}

#[test]
fn disable_is_explicit_and_different_from_provider_default() {
    for (protocol, adapter) in [
        (ProviderProtocol::ChatCompletions, "deepseek"),
        (ProviderProtocol::AnthropicMessages, "messages_effort"),
        (ProviderProtocol::AnthropicMessages, "anthropic_adaptive"),
        (ProviderProtocol::AnthropicMessages, "anthropic_budget"),
        (ProviderProtocol::ChatCompletions, "chat_toggle"),
        (ProviderProtocol::ChatCompletions, "thinking_toggle"),
    ] {
        let base = model(protocol)
            .with_reasoning_capability(capability(adapter, json!([{"id":"off","enabled":false}])))
            .unwrap();
        let default = base.encode(&request()).unwrap();
        assert!(default.get("thinking").is_none());
        assert!(default.get("enable_thinking").is_none());
        let disabled = base
            .with_reasoning_effort("off")
            .unwrap()
            .encode(&request())
            .unwrap();
        if adapter == "chat_toggle" {
            assert_eq!(disabled["enable_thinking"], false);
        } else {
            assert_eq!(disabled["thinking"], json!({"type":"disabled"}));
        }
        assert!(disabled.get("reasoning_effort").is_none());
        assert!(disabled.get("output_config").is_none());
    }
    let toggle = model(ProviderProtocol::ChatCompletions)
        .with_reasoning_capability(capability(
            "chat_toggle",
            json!([{"id":"on","enabled":true}]),
        ))
        .unwrap()
        .with_reasoning_effort("on")
        .unwrap();
    assert_eq!(toggle.encode(&request()).unwrap()["enable_thinking"], true);
    let thinking_toggle = model(ProviderProtocol::ChatCompletions)
        .with_reasoning_capability(capability(
            "thinking_toggle",
            json!([{"id":"on","enabled":true}]),
        ))
        .unwrap()
        .with_reasoning_effort("on")
        .unwrap()
        .encode(&request())
        .unwrap();
    assert_eq!(thinking_toggle["thinking"], json!({"type":"enabled"}));
    assert!(thinking_toggle.get("reasoning_effort").is_none());
    assert!(thinking_toggle.get("enable_thinking").is_none());
}

#[test]
fn budget_choices_respect_output_caps_and_compactor_reserves() {
    let base = model(ProviderProtocol::AnthropicMessages)
        .with_reasoning_capability(capability(
            "anthropic_budget",
            json!([
                {"id":"short","budget_tokens":1024},
                {"id":"long","budget_tokens":4096},
                {"id":"too_large","budget_tokens":8192}
            ]),
        ))
        .unwrap();
    assert_eq!(base.supported_reasoning_efforts(), vec!["short", "long"]);
    assert!(base.with_reasoning_effort("too_large").is_err());
    let selected = base.with_reasoning_effort("long").unwrap();
    assert_eq!(
        selected.encode(&request()).unwrap()["thinking"]["budget_tokens"],
        4096
    );
    let mut compact = request();
    compact.max_output_tokens = Some(2048);
    assert_eq!(
        selected.encode(&compact).unwrap()["thinking"]["budget_tokens"],
        2047
    );
    compact.max_output_tokens = Some(1024);
    assert!(selected.encode(&compact).unwrap().get("thinking").is_none());
}

#[test]
fn declarations_reject_ambiguous_fields_duplicates_and_wrong_protocols() {
    let chat = model(ProviderProtocol::ChatCompletions);
    for bad in [
        capability(
            "effort",
            json!([{"id":"high","effort":"high","enabled":true}]),
        ),
        capability(
            "effort",
            json!([{"id":"high","effort":"high"},{"id":"high","effort":"low"}]),
        ),
        capability("effort", json!([{"id":"default","effort":"high"}])),
        capability("effort", json!([{"id":"high","effort":"HIGH"}])),
        capability("effort", json!([])),
        capability("none", json!([{"id":"off"}])),
        capability(
            "deepseek",
            json!([{"id":"off","enabled":false,"effort":"low"}]),
        ),
        capability(
            "thinking_toggle",
            json!([{"id":"on","enabled":true,"effort":"low"}]),
        ),
        capability(
            "messages_effort_only",
            json!([{"id":"high","effort":"high"}]),
        ),
        capability("chat_toggle", json!([{"id":"on","effort":"low"}])),
        capability("anthropic_adaptive", json!([{"id":"high","effort":"high"}])),
    ] {
        assert!(chat.clone().with_reasoning_capability(bad).is_err());
    }
    let messages = model(ProviderProtocol::AnthropicMessages);
    for bad in [
        capability(
            "messages_effort_only",
            json!([{"id":"high","effort":"high","enabled":true}]),
        ),
        capability(
            "messages_effort_only",
            json!([{"id":"off","enabled":false}]),
        ),
        capability(
            "messages_effort_only",
            json!([{"id":"high","effort":"high","budget_tokens":1024}]),
        ),
        capability("thinking_toggle", json!([{"id":"on","enabled":true}])),
    ] {
        assert!(messages.clone().with_reasoning_capability(bad).is_err());
    }
    assert!(serde_json::from_value::<ReasoningCapability>(json!({"source":"manual","adapter":"effort","options":[{"id":"high","effort":"high","body":{"secret":"bad"}}]})).is_err());
}

#[test]
fn stored_declarations_override_presets_including_an_explicit_empty_capability() {
    let base = model(ProviderProtocol::Responses);
    let empty = base
        .clone()
        .with_reasoning_capability(ReasoningCapability::default())
        .unwrap();
    assert!(empty.supported_reasoning_efforts().is_empty());
    assert!(empty.with_reasoning_effort("high").is_err());
    let stored: ReasoningCapability = serde_json::from_value(json!({
        "source":"catalog", "adapter":"effort", "options":[{"id":"max", "effort":"max"}]
    }))
    .unwrap();
    let old = base.with_reasoning_capability(stored).unwrap();
    assert_eq!(old.supported_reasoning_efforts(), vec!["max"]);
    assert!(old.with_reasoning_effort("high").is_err());
    assert_eq!(
        old.with_reasoning_effort("max")
            .unwrap()
            .encode(&request())
            .unwrap()["reasoning"]["effort"],
        "max"
    );
}

#[test]
fn legacy_explicit_profile_retains_its_dialect_default_and_identity() {
    let original = HttpModel::new(ProviderConfig {
        protocol: ProviderProtocol::Responses,
        base_url: "https://api.openai.com/v1".into(),
        api_key: "fixture".into(),
        model: "gpt-6-luna".into(),
        context_window: 1_050_000,
        max_output_tokens: 8192,
        reasoning: ReasoningMode::Effort {
            effort: "high".into(),
        },
        supports_images: false,
    })
    .unwrap();
    assert_eq!(
        original.reasoning_capability().source,
        ReasoningSource::Legacy
    );
    assert_eq!(
        original.supported_reasoning_efforts(),
        vec!["low", "medium", "high"]
    );
    assert_eq!(
        original.encode(&request()).unwrap()["reasoning"]["effort"],
        "high"
    );
    assert_eq!(
        original
            .with_reasoning_effort("low")
            .unwrap()
            .encode(&request())
            .unwrap()["reasoning"]["effort"],
        "low"
    );
    assert!(
        original
            .identity
            .format
            .contains("Effort { effort: \"high\" }")
    );
}

#[test]
fn declared_mapping_reuses_legacy_replay_identity_for_unchanged_wire_semantics() {
    for (protocol, mode, adapter, option) in [
        (
            ProviderProtocol::Responses,
            ReasoningMode::Effort {
                effort: "high".into(),
            },
            "effort",
            json!({"id":"high","label":"High","effort":"high"}),
        ),
        (
            ProviderProtocol::ChatCompletions,
            ReasoningMode::DeepSeek {
                effort: "high".into(),
            },
            "deepseek",
            json!({"id":"high","effort":"high"}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            ReasoningMode::AnthropicBudget { tokens: 4096 },
            "anthropic_budget",
            json!({"id":"medium","budget_tokens":4096}),
        ),
    ] {
        let mut legacy_config = ProviderConfig {
            protocol,
            base_url: "https://relay.example/v1".into(),
            api_key: "fixture".into(),
            model: "gpt-6-luna".into(),
            context_window: 32000,
            max_output_tokens: 8192,
            reasoning: mode,
            supports_images: false,
        };
        let old = HttpModel::new(legacy_config.clone()).unwrap();
        legacy_config.reasoning = ReasoningMode::Off;
        let selected_id = option["id"].as_str().unwrap().to_owned();
        let new = HttpModel::new(legacy_config)
            .unwrap()
            .with_reasoning_capability(capability(adapter, json!([option])))
            .unwrap()
            .with_reasoning_effort(&selected_id)
            .unwrap();
        assert_eq!(old.identity, new.identity);
        assert_eq!(
            old.encode(&request()).unwrap(),
            new.encode(&request()).unwrap()
        );
    }
    let unknown = model(ProviderProtocol::ChatCompletions);
    assert!(unknown.with_reasoning_effort("ultra").is_err());
    let restored = unknown.restore_legacy_reasoning_effort("high").unwrap();
    assert_eq!(
        restored.supported_reasoning_efforts(),
        vec!["low", "medium", "high"]
    );
    assert_eq!(
        restored.encode(&request()).unwrap()["reasoning_effort"],
        "high"
    );
    assert!(unknown.restore_legacy_reasoning_effort("max").is_err());
    assert!(
        unknown
            .with_reasoning_capability(ReasoningCapability::default())
            .unwrap()
            .restore_legacy_reasoning_effort("high")
            .is_err()
    );
}
