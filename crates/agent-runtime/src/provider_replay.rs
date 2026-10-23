use std::{error::Error, fmt};

use crate::{
    ContentBlock, MessageContext, ModelOutput, ModelRequest, ModelRole, ProviderContextIdentity,
    ProviderReplayScope, ProviderResponseState, ProviderStateError,
};

#[derive(Debug)]
pub enum ProviderReplayError {
    IdentityMismatch,
    MissingState,
    PrefixChanged,
    InvalidWindow,
    InvalidResponse(ProviderStateError),
    Encoding(serde_json::Error),
}

impl fmt::Display for ProviderReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityMismatch => f.write_str("provider response state belongs to an incompatible provider, model, or adapter format"),
            Self::MissingState => f.write_str("model requires durable provider response state, but it is missing"),
            Self::PrefixChanged => f.write_str("prefix-bound provider state cannot be replayed after changing instructions, tools, or prior messages; use provider-native compaction"),
            Self::InvalidWindow => f.write_str("provider continuation requires a complete nonempty window and its input token count"),
            Self::InvalidResponse(error) => write!(f, "invalid provider response state: {error}"),
            Self::Encoding(_) => f.write_str("could not encode provider replay prefix"),
        }
    }
}

impl Error for ProviderReplayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidResponse(error) => Some(error),
            Self::Encoding(error) => Some(error),
            _ => None,
        }
    }
}

impl ModelRequest {
    /// Validate protocol-independent identity and prefix invariants. Call
    /// `validate_provider_state_with` to additionally validate adapter payloads.
    pub fn validate_provider_state(
        &self,
        identity: Option<&ProviderContextIdentity>,
        required: bool,
    ) -> Result<(), ProviderReplayError> {
        self.validate_provider_state_with(identity, required, |state, _| state.validate_envelope())
    }

    /// Validate immutable history, then ask its owning adapter to verify the
    /// original payload against each complete ordered assistant message.
    pub fn validate_provider_state_with(
        &self,
        identity: Option<&ProviderContextIdentity>,
        required: bool,
        validator: impl Fn(&ProviderResponseState, &[ContentBlock]) -> Result<(), ProviderStateError>,
    ) -> Result<(), ProviderReplayError> {
        if let Some(window) = &self.provider_context {
            if identity != Some(&window.identity) {
                return Err(ProviderReplayError::IdentityMismatch);
            }
            if let Some(binding) = &window.prefix_fingerprint
                && binding != &self.replay_fixed_fingerprint()?
            {
                return Err(ProviderReplayError::PrefixChanged);
            }
        }
        for (index, message) in self.messages.iter().enumerate() {
            if message.role != ModelRole::Assistant {
                continue;
            }
            let Some(state) = &message.provider_state else {
                if required {
                    return Err(ProviderReplayError::MissingState);
                }
                continue;
            };
            if identity != Some(&state.identity) {
                return Err(ProviderReplayError::IdentityMismatch);
            }
            state
                .validate_envelope()
                .map_err(ProviderReplayError::InvalidResponse)?;
            validator(state, &message.content).map_err(ProviderReplayError::InvalidResponse)?;
            if state.scope == ProviderReplayScope::Prefix
                && state.prefix_fingerprint.as_deref()
                    != Some(self.replay_prefix_fingerprint(index)?.as_str())
                && !(state.version == 0
                    && state.prefix_fingerprint.is_some()
                    && state.prefix_fingerprint == self.legacy_replay_prefix_fingerprint(index)?)
            {
                return Err(ProviderReplayError::PrefixChanged);
            }
        }
        Ok(())
    }

    pub(crate) fn has_prefix_bound_state(&self) -> bool {
        self.provider_context
            .as_ref()
            .is_some_and(|window| window.prefix_fingerprint.is_some())
            || self.messages.iter().any(|message| {
                message
                    .provider_state
                    .as_ref()
                    .is_some_and(|state| state.scope == ProviderReplayScope::Prefix)
            })
    }

    pub(crate) fn replay_fixed_fingerprint(&self) -> Result<String, ProviderReplayError> {
        fingerprint(&(&self.instructions, &self.tools))
    }

    pub(crate) fn replay_prefix_fingerprint(
        &self,
        count: usize,
    ) -> Result<String, ProviderReplayError> {
        // Stable, versioned integrity key, not an authentication primitive.
        // Ignore output limits; they are not part of the provider's input prefix.
        fingerprint(&(
            &self.instructions,
            &self.tools,
            &self.provider_context,
            &self.messages[..count.min(self.messages.len())],
        ))
    }

    // Version 0 envelopes were bound before ordered content blocks existed.
    // Rebuild that serialization only when the prefix can be represented
    // losslessly. Never apply this fallback to new state or discard new blocks.
    fn legacy_replay_prefix_fingerprint(
        &self,
        count: usize,
    ) -> Result<Option<String>, ProviderReplayError> {
        let Some(messages) = legacy_messages(&self.messages[..count.min(self.messages.len())])
        else {
            return Ok(None);
        };
        fingerprint(&(
            &self.instructions,
            &self.tools,
            &self.provider_context,
            messages,
        ))
        .map(Some)
    }
}

/// Exact pre-content-block serialization, retained only for integrity checks.
#[derive(serde::Serialize)]
enum LegacyMessage<'a> {
    User {
        content: String,
    },
    ProtectedContext {
        content: String,
    },
    ContextSummary {
        checkpoint_id: &'a str,
        content: String,
    },
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_state: Option<&'a ProviderResponseState>,
    },
    ToolCall {
        call_id: &'a str,
        tool_name: &'a str,
        arguments: &'a str,
    },
    ToolResult {
        call_id: &'a str,
        output: &'a str,
    },
    ToolError {
        call_id: &'a str,
        message: &'a str,
    },
}

fn legacy_messages(messages: &[crate::ModelMessage]) -> Option<Vec<LegacyMessage<'_>>> {
    let mut legacy = Vec::new();
    for message in messages {
        if message
            .provider_state
            .as_ref()
            .is_some_and(|state| state.version != 0)
        {
            return None;
        }
        match message.role {
            ModelRole::User if message.provider_state.is_none() => {
                let content = legacy_text(&message.content)?;
                legacy.push(match &message.context {
                    None => LegacyMessage::User { content },
                    Some(MessageContext::Protected) => LegacyMessage::ProtectedContext { content },
                    Some(MessageContext::Summary { checkpoint_id }) => {
                        LegacyMessage::ContextSummary {
                            checkpoint_id,
                            content,
                        }
                    }
                });
            }
            ModelRole::Assistant if message.context.is_none() => {
                let call_start = message
                    .content
                    .iter()
                    .position(|block| matches!(block, ContentBlock::ToolCall { .. }))
                    .unwrap_or(message.content.len());
                let content = legacy_text(&message.content[..call_start])?;
                legacy.push(LegacyMessage::Assistant {
                    content,
                    provider_state: message.provider_state.as_ref(),
                });
                for block in &message.content[call_start..] {
                    let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = block
                    else {
                        return None;
                    };
                    legacy.push(LegacyMessage::ToolCall {
                        call_id: id,
                        tool_name: name,
                        arguments,
                    });
                }
            }
            ModelRole::Tool if message.context.is_none() && message.provider_state.is_none() => {
                if message.content.is_empty() {
                    return None;
                }
                for block in &message.content {
                    let ContentBlock::ToolResult {
                        call_id,
                        content,
                        is_error,
                        images,
                    } = block
                    else {
                        return None;
                    };
                    if !images.is_empty() {
                        return None;
                    }
                    legacy.push(if *is_error {
                        LegacyMessage::ToolError {
                            call_id,
                            message: content,
                        }
                    } else {
                        LegacyMessage::ToolResult {
                            call_id,
                            output: content,
                        }
                    });
                }
            }
            _ => return None,
        }
    }
    Some(legacy)
}

fn legacy_text(content: &[ContentBlock]) -> Option<String> {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn fingerprint(value: &impl serde::Serialize) -> Result<String, ProviderReplayError> {
    let encoded = serde_json::to_vec(value).map_err(ProviderReplayError::Encoding)?;
    let mut a = 0xcbf2_9ce4_8422_2325_u64;
    let mut b = 0x8422_2325_cbf2_9ce4_u64;
    for byte in b"provider-replay-prefix-v1".iter().chain(&encoded) {
        a = (a ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        b = (b ^ u64::from(*byte).rotate_left(1)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("v1:{a:016x}{b:016x}"))
}

impl ModelOutput {
    pub(crate) fn bind_provider_state(
        &mut self,
        identity: Option<&ProviderContextIdentity>,
        required: bool,
        prefix: &str,
    ) -> Result<(), ProviderReplayError> {
        if self.context_compacted && self.provider_context.is_none() {
            return Err(ProviderReplayError::InvalidWindow);
        }
        if let Some(window) = &self.provider_context {
            if identity != Some(&window.identity) {
                return Err(ProviderReplayError::IdentityMismatch);
            }
            if window.items.is_empty() || window.input_tokens == 0 {
                return Err(ProviderReplayError::InvalidWindow);
            }
        }
        let Some(state) = &mut self.provider_state else {
            return if required {
                Err(ProviderReplayError::MissingState)
            } else {
                Ok(())
            };
        };
        if identity != Some(&state.identity) {
            return Err(ProviderReplayError::IdentityMismatch);
        }
        state
            .validate_envelope()
            .map_err(ProviderReplayError::InvalidResponse)?;
        state.prefix_fingerprint =
            (state.scope == ProviderReplayScope::Prefix).then(|| prefix.to_owned());
        Ok(())
    }
}
