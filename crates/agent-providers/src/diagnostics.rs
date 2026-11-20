use serde_json::Value;
use std::fmt;

pub(super) const OPAQUE_ROUTING_REJECTION: &str =
    "the requested capabilities are unsupported by every eligible channel: opaque_pass_through";
pub(super) const TOOLS_OPAQUE_ROUTING_REJECTION: &str = "the requested capabilities are unsupported by every eligible channel: function_tools, opaque_pass_through";
const OPAQUE_TOOLS_ROUTING_REJECTION: &str = "the requested capabilities are unsupported by every eligible channel: opaque_pass_through, function_tools";
pub(super) const TOOLS_ROUTING_REJECTION: &str =
    "the requested capabilities are unsupported by every eligible channel: function_tools";

pub(super) fn unsupported_opaque_routing(message: &Value) -> bool {
    matches!(
        message.as_str(),
        Some(
            OPAQUE_ROUTING_REJECTION
                | TOOLS_OPAQUE_ROUTING_REJECTION
                | OPAQUE_TOOLS_ROUTING_REJECTION
        )
    )
}

/// Safe diagnostics that may be persisted in run events and displayed to users.
/// A provider can echo credentials or request contents in *any* error field, so
/// character filtering or truncating a free-form message is not sufficient.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderErrorDetails {
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub param: Option<String>,
    pub message: Option<&'static str>,
}

impl ProviderErrorDetails {
    pub(super) fn from_error(error: &Value) -> Self {
        let mut details = Self {
            code: error["code"]
                .as_str()
                .and_then(known_code)
                .map(str::to_owned),
            error_type: error["type"]
                .as_str()
                .and_then(known_code)
                .map(str::to_owned),
            param: error["param"]
                .as_str()
                .filter(|s| safe_param(s))
                .map(str::to_owned),
            message: None,
        };
        let raw = error["message"]
            .as_str()
            .or_else(|| error.as_str())
            .unwrap_or("");
        // Extract field names only from familiar provider templates. Never copy
        // values, arbitrary prose, request IDs, URLs, or quoted user input.
        for (prefix, message) in [
            (
                "Unsupported parameter: '",
                "parameter is not supported by this model",
            ),
            (
                "Unknown parameter: '",
                "parameter is not recognized by this provider",
            ),
            (
                "Unrecognized request argument supplied: ",
                "parameter is not recognized by this provider",
            ),
            (
                "Missing required parameter: '",
                "a required parameter is missing",
            ),
            ("Invalid value for '", "parameter value is invalid"),
            ("Invalid parameter: '", "parameter value is invalid"),
            (
                "Unsupported value: '",
                "parameter value is not supported by this model",
            ),
        ] {
            if let Some(rest) = raw.strip_prefix(prefix) {
                let field = rest.split(['\'', ' ', ':']).next().unwrap_or("");
                if safe_param(field) {
                    details.param.get_or_insert_with(|| field.to_owned());
                    details.message = Some(message);
                    break;
                }
            }
        }
        if details.message.is_none() {
            details.message = match details.code.as_deref().or(details.error_type.as_deref()) {
                Some("unsupported_parameter") => Some("parameter is not supported by this model"),
                Some("unsupported_value") => Some("parameter value is not supported by this model"),
                Some("missing_required_parameter") => Some("a required parameter is missing"),
                Some("invalid_value" | "invalid_parameter") => Some("parameter value is invalid"),
                Some("response_conversion_failed") => Some(
                    "provider gateway could not convert the upstream response; use the model's native API protocol",
                ),
                Some("timeout_error") => Some("provider timed out while processing the request"),
                Some("conflict_error") => Some("request conflicts with the current resource state"),
                Some("request_too_large") => Some("request exceeds the provider's byte-size limit"),
                Some("model_not_found" | "model_not_available") => {
                    Some("model is unavailable or this account has no access")
                }
                Some("insufficient_quota") => Some("provider quota is exhausted"),
                Some("invalid_api_key" | "authentication_error") => {
                    Some("provider credentials were rejected")
                }
                Some("context_length_exceeded" | "context_window_exceeded" | "prompt_too_long") => {
                    Some("request exceeds the model context limit")
                }
                _ => None,
            };
        }
        if details.message.is_none() {
            details.message = match raw.trim() {
                OPAQUE_ROUTING_REJECTION => Some(
                    "provider routing does not support the requested protocol capability: opaque_pass_through",
                ),
                TOOLS_OPAQUE_ROUTING_REJECTION | OPAQUE_TOOLS_ROUTING_REJECTION => Some(
                    "provider routing does not support the requested capability combination: function_tools, opaque_pass_through",
                ),
                TOOLS_ROUTING_REJECTION => Some(
                    "provider routing does not support the requested capability: function_tools",
                ),
                "Instructions are required" | "Instructions are required." => {
                    details
                        .param
                        .get_or_insert_with(|| "instructions".to_owned());
                    Some("a required parameter is missing")
                }
                "Input is required" | "Input is required." => {
                    details.param.get_or_insert_with(|| "input".to_owned());
                    Some("a required parameter is missing")
                }
                _ => None,
            };
        }
        details
    }
}

fn known_code(value: &str) -> Option<&str> {
    matches!(
        value,
        "invalid_request_error"
            | "response_conversion_failed"
            | "invalid_request"
            | "invalid_argument"
            | "bad_request"
            | "bad_request_error"
            | "validation_error"
            | "timeout_error"
            | "conflict_error"
            | "request_too_large"
            | "unsupported_parameter"
            | "unsupported_value"
            | "invalid_parameter"
            | "invalid_value"
            | "missing_required_parameter"
            | "model_not_found"
            | "model_not_available"
            | "context_length_exceeded"
            | "context_window_exceeded"
            | "prompt_too_long"
            | "rate_limit_exceeded"
            | "rate_limit_error"
            | "insufficient_quota"
            | "server_error"
            | "internal_server_error"
            | "api_error"
            | "overloaded_error"
            | "authentication_error"
            | "invalid_api_key"
            | "permission_error"
            | "permission_denied"
            | "not_found_error"
            | "content_policy_violation"
            | "content_filter"
            | "billing_hard_limit_reached"
    )
    .then_some(value)
}

fn safe_param(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.split(['.', '[', ']']).all(|part| {
            part.is_empty()
                || (part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
                || matches!(
                    part,
                    "model"
                        | "messages"
                        | "input"
                        | "instructions"
                        | "role"
                        | "content"
                        | "type"
                        | "text"
                        | "image_url"
                        | "image"
                        | "detail"
                        | "tools"
                        | "function"
                        | "name"
                        | "description"
                        | "parameters"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "strict"
                        | "tool_choice"
                        | "tool_calls"
                        | "tool_call_id"
                        | "call_id"
                        | "arguments"
                        | "output"
                        | "parallel_tool_calls"
                        | "reasoning"
                        | "effort"
                        | "summary"
                        | "reasoning_effort"
                        | "thinking"
                        | "budget_tokens"
                        | "max_tokens"
                        | "max_output_tokens"
                        | "max_completion_tokens"
                        | "temperature"
                        | "top_p"
                        | "top_k"
                        | "stream"
                        | "stream_options"
                        | "include_usage"
                        | "store"
                        | "include"
                        | "previous_response_id"
                        | "response_format"
                        | "format"
                        | "json_schema"
                        | "schema"
                        | "verbosity"
                        | "truncation"
                        | "service_tier"
                        | "stop"
                        | "seed"
                        | "n"
                        | "logprobs"
                        | "top_logprobs"
                        | "frequency_penalty"
                        | "presence_penalty"
                        | "metadata"
                        | "user"
                )
        })
        && value.bytes().any(|b| b.is_ascii_alphabetic())
}

impl fmt::Display for ProviderErrorDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in [
            ("code", self.code.as_deref()),
            ("type", self.error_type.as_deref()),
            ("param", self.param.as_deref()),
            ("message", self.message),
        ] {
            if let Some(value) = value {
                write!(f, "; {key}={value}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explains_gateway_conversion_failure_without_echoing_provider_text() {
        let details = ProviderErrorDetails::from_error(&json!({
            "code":"response_conversion_failed", "type":"response_conversion_failed",
            "message":"The gateway could not convert the upstream response. private upstream data"
        }));
        assert_eq!(details.code.as_deref(), Some("response_conversion_failed"));
        assert!(details.to_string().contains("native API protocol"));
        assert!(!details.to_string().contains("private"));
    }

    #[test]
    fn explains_documented_native_errors_without_echoing_provider_text() {
        for (error_type, explanation) in [
            ("timeout_error", "provider timed out while processing the request"),
            ("conflict_error", "request conflicts with the current resource state"),
            ("request_too_large", "request exceeds the provider's byte-size limit"),
        ] {
            let details = ProviderErrorDetails::from_error(&json!({
                "type": error_type,
                "message": "private prompt and credentials: secret-test-value",
            }));
            assert_eq!(details.error_type.as_deref(), Some(error_type));
            assert_eq!(details.message, Some(explanation));
            assert!(details.to_string().contains(explanation));
            assert!(!format!("{details:?}{details}").contains("secret-test-value"));
            assert!(!format!("{details:?}{details}").contains("private prompt"));
        }
    }

    #[test]
    fn exposes_actionable_parameter_details_without_echoing_values() {
        let details = ProviderErrorDetails::from_error(&json!({
            "code":"unsupported_value", "type":"invalid_request_error",
            "param":"reasoning.effort",
            "message":"Unsupported value: 'reasoning.effort' does not support secret-test-value; https://private.example?key=secret"
        }));
        assert_eq!(details.code.as_deref(), Some("unsupported_value"));
        assert_eq!(details.param.as_deref(), Some("reasoning.effort"));
        assert_eq!(
            details.message,
            Some("parameter value is not supported by this model")
        );
        assert!(!format!("{details:?}{details}").contains("secret"));
    }

    #[test]
    fn recovers_parameter_from_known_message_without_a_param_field() {
        let details = ProviderErrorDetails::from_error(&json!({
            "type":"invalid_request_error",
            "message":"Unsupported parameter: 'max_output_tokens'. secret prompt echo"
        }));
        assert_eq!(details.param.as_deref(), Some("max_output_tokens"));
        assert_eq!(
            details.message,
            Some("parameter is not supported by this model")
        );
        assert!(!details.to_string().contains("secret"));
    }

    #[test]
    fn arbitrary_provider_fields_and_messages_are_never_retained() {
        for secret in [
            "secret-test-value",
            "sk-live-key",
            "user_private_prompt",
            "https://private.example",
            "Bearer token",
            "input.private_prompt",
            "max_tokens\nsecret",
            "思考原文",
        ] {
            let details = ProviderErrorDetails::from_error(&json!({
                "code":secret, "type":secret, "param":secret,
                "message": format!("Unsupported parameter: '{secret}'.")
            }));
            assert_eq!(details, ProviderErrorDetails::default());
        }
        assert_eq!(
            ProviderErrorDetails::from_error(&json!("raw private prompt")),
            ProviderErrorDetails::default()
        );
        assert_eq!(
            ProviderErrorDetails::from_error(&Value::Null),
            ProviderErrorDetails::default()
        );
    }

    #[test]
    fn accepts_structured_parameter_paths_but_rejects_user_named_properties() {
        assert!(safe_param(
            "tools[0].function.parameters.additionalProperties"
        ));
        assert!(safe_param("messages.0.content.1.type"));
        assert!(!safe_param(
            "tools.0.function.parameters.properties.private_name"
        ));
        assert!(!safe_param("123"));
    }

    #[test]
    fn explains_known_routing_capability_rejection_without_retaining_free_text() {
        let details = ProviderErrorDetails::from_error(&json!({
            "code":"invalid_request", "type":"invalid_request_error", "param":"",
            "message":"the requested capabilities are unsupported by every eligible channel: opaque_pass_through"
        }));
        assert_eq!(
            details.message,
            Some(
                "provider routing does not support the requested protocol capability: opaque_pass_through"
            )
        );
        let unknown = ProviderErrorDetails::from_error(&json!({
            "message":"the requested capabilities are unsupported by every eligible channel: secret-test-value"
        }));
        assert_eq!(unknown, ProviderErrorDetails::default());
    }
}
