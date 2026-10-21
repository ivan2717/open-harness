use agent_core::{
    model_protocol::ContentBlock,
    provider_state::{
        ProviderContextIdentity, ProviderReplayScope, ProviderResponseState, ProviderStateError,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::replay::{CapturedResponse, capture};

/// Wire formats supported by this adapter library. This list is not part of
/// the harness core: another adapter can use any private format key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
    GeminiGenerateContent,
}

impl WireProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiGenerateContent => "gemini_generate_content",
        }
    }

    /// Capture a complete, settled provider response without changing any raw
    /// fields. `replay_tokens` counts reasoning and opaque replay input beyond
    /// portable text/tool calls; it replaces the display reasoning token estimate.
    pub fn capture_state(
        self,
        identity: ProviderContextIdentity,
        scope: ProviderReplayScope,
        payload: Value,
        replay_tokens: usize,
    ) -> Result<ProviderResponseState, ProviderStateError> {
        let state = ProviderResponseState::new(identity, self, scope, payload, replay_tokens)?;
        capture(self, &state.payload)?;
        Ok(state)
    }

    /// Normalize content in its original order. Signatures and opaque items
    /// remain exclusively in the captured state. `response_namespace` must be
    /// stable for this response and unique across the run (for example, the
    /// provider response ID or a durable request/step ID). Missing wire call IDs
    /// are derived from it; persist the resulting blocks rather than decoding
    /// the same response later with a different namespace.
    pub fn decode_content(
        self,
        payload: &Value,
        response_namespace: &str,
    ) -> Result<Vec<ContentBlock>, ProviderStateError> {
        if response_namespace.trim().is_empty() {
            return Err(ProviderStateError::InvalidIdentity);
        }
        let captured: CapturedResponse = capture(self, payload)?;
        Ok(captured.into_content(response_namespace))
    }
}

impl From<WireProtocol> for String {
    fn from(value: WireProtocol) -> Self {
        value.as_str().to_owned()
    }
}

impl TryFrom<&str> for WireProtocol {
    type Error = ProviderStateError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chat_completions" => Ok(Self::ChatCompletions),
            "responses" => Ok(Self::Responses),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            "gemini_generate_content" => Ok(Self::GeminiGenerateContent),
            _ => Err(ProviderStateError::UnsupportedFormat),
        }
    }
}
