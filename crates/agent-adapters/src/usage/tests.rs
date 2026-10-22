use serde_json::{Value, json};

use super::*;

fn decode(protocol: WireProtocol, usage: Value) -> Result<Option<ModelUsage>, UsageDecodeError> {
    let field = match protocol {
        WireProtocol::GeminiGenerateContent => "usageMetadata",
        _ => "usage",
    };
    protocol.decode_usage(&json!({ field: usage }))
}

#[test]
fn missing_usage_and_explicit_zero_are_distinct_for_every_protocol() {
    for (protocol, zero) in [
        (
            WireProtocol::ChatCompletions,
            json!({"prompt_tokens": 0, "completion_tokens": 0}),
        ),
        (
            WireProtocol::Responses,
            json!({"input_tokens": 0, "output_tokens": 0}),
        ),
        (
            WireProtocol::AnthropicMessages,
            json!({"input_tokens": 0, "output_tokens": 0}),
        ),
        (
            WireProtocol::GeminiGenerateContent,
            json!({"promptTokenCount": 0, "totalTokenCount": 0}),
        ),
    ] {
        assert_eq!(protocol.decode_usage(&json!({})), Ok(None));
        assert_eq!(decode(protocol, Value::Null), Ok(None));
        assert_eq!(
            decode(protocol, zero),
            Ok(Some(ModelUsage {
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: None,
            }))
        );
        assert!(decode(protocol, json!({})).is_err());
    }
}

#[test]
fn chat_cache_aliases_and_reasoning_are_subsets_not_additional_tokens() {
    let usage = decode(
        WireProtocol::ChatCompletions,
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 30,
            "total_tokens": 130,
            "prompt_tokens_details": {"cached_tokens": 80},
            "prompt_cache_hit_tokens": 80,
            "prompt_cache_miss_tokens": 20,
            "completion_tokens_details": {"reasoning_tokens": 25}
        }),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        usage,
        ModelUsage {
            input_tokens: 100,
            output_tokens: 30,
            cached_input_tokens: Some(80),
        }
    );

    let legacy = decode(
        WireProtocol::ChatCompletions,
        json!({"prompt_tokens": 100, "completion_tokens": 30, "prompt_cache_hit_tokens": 80}),
    )
    .unwrap();
    assert_eq!(legacy, Some(usage));
}

#[test]
fn responses_preserves_reported_totals_including_hidden_reasoning() {
    assert_eq!(
        decode(
            WireProtocol::Responses,
            json!({
                "input_tokens": 100,
                "output_tokens": 50,
                "total_tokens": 150,
                "input_tokens_details": {"cached_tokens": 90},
                "output_tokens_details": {"reasoning_tokens": 40}
            })
        ),
        Ok(Some(ModelUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: Some(90),
        }))
    );
}

#[test]
fn anthropic_adds_disjoint_input_categories_once() {
    assert_eq!(
        decode(
            WireProtocol::AnthropicMessages,
            json!({
                "input_tokens": 10,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 20,
                "cache_creation": {"ephemeral_5m_input_tokens": 15, "ephemeral_1h_input_tokens": 5},
                "output_tokens": 30
            })
        ),
        Ok(Some(ModelUsage {
            input_tokens: 110,
            output_tokens: 30,
            cached_input_tokens: Some(80),
        }))
    );
}

#[test]
fn gemini_adds_separate_thoughts_and_internal_tool_inputs_but_not_cached_content() {
    assert_eq!(
        decode(
            WireProtocol::GeminiGenerateContent,
            json!({
                "promptTokenCount": 100,
                "cachedContentTokenCount": 80,
                "toolUsePromptTokenCount": 10,
                "candidatesTokenCount": 20,
                "thoughtsTokenCount": 30,
                "totalTokenCount": 160
            })
        ),
        Ok(Some(ModelUsage {
            input_tokens: 110,
            output_tokens: 50,
            cached_input_tokens: Some(80),
        }))
    );
}

#[test]
fn gemini_blocked_response_may_omit_candidate_count() {
    let response = json!({
        "promptFeedback": {"blockReason": "SAFETY"},
        "usageMetadata": {"promptTokenCount": 50, "totalTokenCount": 50}
    });
    assert_eq!(
        WireProtocol::GeminiGenerateContent.decode_usage(&response),
        Ok(Some(ModelUsage {
            input_tokens: 50,
            output_tokens: 0,
            cached_input_tokens: None,
        }))
    );
}

#[test]
fn malformed_usage_never_silently_becomes_zero() {
    let protocol = WireProtocol::ChatCompletions;
    assert!(protocol.decode_usage(&json!([])).is_err());
    for invalid in [json!([]), json!("0"), json!(0)] {
        assert!(decode(protocol, invalid).is_err());
    }
    for invalid in [json!(-1), json!(1.5), json!(1.0), json!("1"), json!(true)] {
        assert_eq!(
            decode(
                protocol,
                json!({"prompt_tokens": invalid, "completion_tokens": 1})
            ),
            Err(UsageDecodeError::InvalidField("prompt_tokens"))
        );
    }
    assert_eq!(
        decode(protocol, json!({"prompt_tokens": 5})),
        Err(UsageDecodeError::MissingField("completion_tokens"))
    );
    assert!(
        decode(
            protocol,
            json!({"prompt_tokens": 5, "completion_tokens": 1, "prompt_tokens_details": "invalid"})
        )
        .is_err()
    );
}

#[test]
fn conflicting_subsets_and_reported_totals_are_rejected() {
    for invalid in [
        json!({"prompt_tokens_details": {"cached_tokens": 11}}),
        json!({"prompt_tokens_details": {"cached_tokens": 2}, "prompt_cache_hit_tokens": 3}),
        json!({"prompt_cache_hit_tokens": 2, "prompt_cache_miss_tokens": 9}),
        json!({"completion_tokens_details": {"reasoning_tokens": 6}}),
        json!({"total_tokens": 20}),
        json!({"prompt_cache_hit_tokens": -1}),
    ] {
        let mut usage = json!({"prompt_tokens": 10, "completion_tokens": 5});
        usage
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        assert!(decode(WireProtocol::ChatCompletions, usage).is_err());
    }
    assert!(
        decode(
            WireProtocol::Responses,
            json!({"input_tokens": 10, "output_tokens": 5, "input_tokens_details": {"cached_tokens": 11}})
        )
        .is_err()
    );
    // Cache is a subset of the original prompt, not of prompt + internal tools.
    assert!(
        decode(
            WireProtocol::GeminiGenerateContent,
            json!({"promptTokenCount": 10, "toolUsePromptTokenCount": 100, "cachedContentTokenCount": 11})
        )
        .is_err()
    );
}

#[test]
fn disjoint_categories_cannot_wrap_or_saturate_into_a_valid_sample() {
    for (protocol, usage) in [
        (
            WireProtocol::AnthropicMessages,
            json!({"input_tokens": usize::MAX, "cache_read_input_tokens": 1, "output_tokens": 0}),
        ),
        (
            WireProtocol::GeminiGenerateContent,
            json!({"promptTokenCount": 0, "candidatesTokenCount": usize::MAX, "thoughtsTokenCount": 1}),
        ),
        (
            WireProtocol::ChatCompletions,
            json!({"prompt_tokens": usize::MAX, "completion_tokens": 1, "total_tokens": 0}),
        ),
    ] {
        assert_eq!(decode(protocol, usage), Err(UsageDecodeError::Overflow));
    }
}
