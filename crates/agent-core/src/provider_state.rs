//! Lossless provider response state, separate from the visible conversation.
//!
//! Payloads are replayed by their originating protocol adapter, never converted
//! to ordinary text. Adapters must choose `Prefix` for state bound to earlier
//! instructions, tools, or messages. `Response` requires known model-specific
//! compatibility.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderContextIdentity {
    pub provider: String,
    pub model: String,
    /// Versioned adapter/wire format, including relevant compatibility modes.
    pub format: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderContextWindow {
    pub identity: ProviderContextIdentity,
    /// The complete ordered input window returned by the provider.
    pub items: Vec<Value>,
    /// Provider-reported or provider-tokenized cost of these items alone.
    /// Instructions, tool schemas, and appended messages are counted separately.
    /// Ciphertext character length is never a substitute for this count.
    pub input_tokens: usize,
    /// Fixed instructions/tools binding for prefix-bound state inside the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_fingerprint: Option<String>,
}

impl fmt::Debug for ProviderContextWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderContextWindow")
            .field("identity", &self.identity)
            .field("items", &"<redacted>")
            .field("input_tokens", &self.input_tokens)
            .field("prefix_fingerprint", &self.prefix_fingerprint)
            .finish()
    }
}

/// A complete provider continuation saved atomically with its response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderContinuation {
    pub window: ProviderContextWindow,
    pub compacted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReplayScope {
    /// Bound to the original response's visible content and ordered calls.
    Response,
    /// Also bound to the exact request prefix that produced the response.
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResponseState {
    pub identity: ProviderContextIdentity,
    /// Adapter-owned format key. Open-ended so new protocols need no core changes.
    /// Existing snake_case protocol names remain valid persisted keys.
    pub protocol: String,
    /// Version of the opaque state envelope, interpreted by the owning adapter.
    /// Missing versions in historical records deserialize as legacy version 0.
    #[serde(default, skip_serializing_if = "is_legacy_version")]
    pub version: u32,
    pub scope: ProviderReplayScope,
    /// Complete adapter-owned response state. Unknown fields and opaque strings
    /// remain untouched; only the owning adapter interprets this value.
    pub payload: Value,
    /// Additional replay input tokens beyond portable text/tool calls, including
    /// replayed reasoning. Counters skip display reasoning when state is present
    /// to avoid double counting. Never infer this cost from ciphertext length.
    pub replay_tokens: usize,
    /// Filled by the runtime for `Prefix` state before durable persistence.
    pub prefix_fingerprint: Option<String>,
}

impl fmt::Debug for ProviderResponseState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponseState")
            .field("identity", &self.identity)
            .field("protocol", &self.protocol)
            .field("version", &self.version)
            .field("scope", &self.scope)
            .field("payload", &"<redacted>")
            .field("replay_tokens", &self.replay_tokens)
            .field("prefix_fingerprint", &self.prefix_fingerprint)
            .finish()
    }
}

const fn initial_state_version() -> u32 {
    1
}

fn is_legacy_version(version: &u32) -> bool {
    *version == 0
}

impl ProviderResponseState {
    /// Construct an opaque envelope. The owning adapter validates the payload;
    /// the core deliberately does not inspect or select a wire protocol.
    pub fn new(
        identity: ProviderContextIdentity,
        protocol: impl Into<String>,
        scope: ProviderReplayScope,
        payload: Value,
        replay_tokens: usize,
    ) -> Result<Self, ProviderStateError> {
        let state = Self {
            identity,
            protocol: protocol.into(),
            version: initial_state_version(),
            scope,
            payload,
            replay_tokens,
            prefix_fingerprint: None,
        };
        state.validate_envelope()?;
        Ok(state)
    }

    /// Validate protocol-independent metadata, including deserialized envelopes.
    /// Version support and payload/response binding belong to the adapter.
    pub fn validate_envelope(&self) -> Result<(), ProviderStateError> {
        if [
            &self.identity.provider,
            &self.identity.model,
            &self.identity.format,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return Err(ProviderStateError::InvalidIdentity);
        }
        if self.protocol.trim().is_empty() {
            return Err(ProviderStateError::UnsupportedFormat);
        }
        Ok(())
    }
}

/// Errors deliberately exclude provider content, signatures, and arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStateError {
    InvalidIdentity,
    UnsupportedFormat,
    InvalidVersion,
    InvalidPayload,
    UnsupportedToolCall,
    ContentMismatch,
    ToolCallsMismatch,
}

impl fmt::Display for ProviderStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentity => "provider response state identity is invalid",
            Self::UnsupportedFormat => {
                "provider response state format is unsupported by this adapter"
            }
            Self::InvalidVersion => "provider response state version is unsupported",
            Self::InvalidPayload => "provider response state payload is invalid",
            Self::UnsupportedToolCall => "provider response contains an unsupported tool call",
            Self::ContentMismatch => "provider response state does not match visible content",
            Self::ToolCallsMismatch => "provider response state does not match ordered tool calls",
        })
    }
}

impl Error for ProviderStateError {}

#[cfg(test)]
mod tests;
