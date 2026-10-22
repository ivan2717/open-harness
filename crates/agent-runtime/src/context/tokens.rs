use crate::{ContentBlock, MessageContext, ModelRequest};
use std::{error::Error, fmt};

/// Estimates the complete input cost, including instructions and tool schemas.
/// Implementations must count `provider_context.input_tokens` without estimating
/// opaque payload contents. Generated output is reserved separately by policy.
pub trait TokenCounter: Send + Sync {
    fn count_tokens(&self, request: &ModelRequest) -> usize;

    /// Stable identity for this algorithm and configuration. Opt in only when
    /// estimates can be compared across process restarts. Unidentified custom
    /// counters still support exact-prefix calibration.
    fn estimator_identity(&self) -> Option<String> {
        None
    }

    /// Counts the same instructions, schemas and provider context, with only the
    /// first `message_count` messages. Override this to avoid copying the prefix.
    fn count_prefix_tokens(&self, request: &ModelRequest, message_count: usize) -> usize {
        if message_count >= request.messages.len() {
            return self.count_tokens(request);
        }
        self.count_tokens(&ModelRequest {
            instructions: request.instructions.clone(),
            messages: request.messages[..message_count].to_vec(),
            tools: request.tools.clone(),
            max_output_tokens: request.max_output_tokens,
            provider_context: request.provider_context.clone(),
        })
    }
}

/// A local heuristic calibrated by the context manager against response usage.
///
/// The default estimates one token per four UTF-8 bytes, with framing allowances.
/// It is a rough estimate, not an exact count. `conservative` selects one token
/// per byte instead. `new` retains the explicit character-ratio mode, without framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApproximateTokenCounter {
    units_per_token: usize,
    count_bytes: bool,
}

impl ApproximateTokenCounter {
    pub fn new(chars_per_token: usize) -> Result<Self, ApproximateTokenCounterError> {
        if chars_per_token == 0 {
            return Err(ApproximateTokenCounterError::ZeroCharsPerToken);
        }
        Ok(Self {
            units_per_token: chars_per_token,
            count_bytes: false,
        })
    }

    /// Counts one token per UTF-8 byte, plus request/message/tool framing.
    pub fn conservative() -> Self {
        Self {
            units_per_token: 1,
            count_bytes: true,
        }
    }

    /// The configured unit ratio. Units are characters for `new`, and UTF-8
    /// bytes for the default and `conservative` modes.
    pub fn chars_per_token(&self) -> usize {
        self.units_per_token
    }

    pub fn is_conservative(&self) -> bool {
        self.count_bytes && self.units_per_token == 1
    }

    fn text_units(&self, text: &str) -> usize {
        if self.count_bytes {
            text.len()
        } else {
            text.chars().count()
        }
    }

    fn count_request_prefix(&self, request: &ModelRequest, message_count: usize) -> usize {
        let mut units = self.text_units(&request.instructions);
        let messages = &request.messages[..message_count.min(request.messages.len())];

        for message in messages {
            if let Some(MessageContext::Summary { checkpoint_id }) = &message.context {
                units = units.saturating_add(self.text_units(checkpoint_id));
            }
            for block in &message.content {
                let cost = match block {
                    ContentBlock::Text { text } => self.text_units(text),
                    // The adapter's declared replay cost already includes
                    // reasoning; counting its presentation again duplicates it.
                    ContentBlock::Reasoning { text } => {
                        if message.provider_state.is_some() {
                            0
                        } else {
                            self.text_units(text)
                        }
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => self
                        .text_units(id)
                        .saturating_add(self.text_units(name))
                        .saturating_add(self.text_units(arguments)),
                    ContentBlock::ToolResult {
                        call_id, content, ..
                    } => self
                        .text_units(call_id)
                        .saturating_add(self.text_units(content)),
                };
                units = units.saturating_add(cost);
            }
        }

        for tool in &request.tools {
            units = units
                .saturating_add(self.text_units(&tool.name))
                .saturating_add(self.text_units(&tool.description))
                .saturating_add(self.text_units(&tool.input_schema));
        }

        let framing = if self.count_bytes {
            16_usize
                .saturating_add(messages.len().saturating_mul(8))
                .saturating_add(request.tools.len().saturating_mul(16))
        } else {
            0
        };
        let response_state_tokens = messages.iter().fold(0usize, |total, message| {
            total.saturating_add(
                message
                    .provider_state
                    .as_ref()
                    .map_or(0, |state| state.replay_tokens),
            )
        });
        let opaque_tokens = request
            .provider_context
            .as_ref()
            .map_or(0, |context| context.input_tokens);
        units
            .div_ceil(self.units_per_token)
            .saturating_add(framing)
            .saturating_add(opaque_tokens)
            .saturating_add(response_state_tokens)
            .max(1)
    }
}

impl Default for ApproximateTokenCounter {
    fn default() -> Self {
        Self {
            units_per_token: 4,
            count_bytes: true,
        }
    }
}

impl TokenCounter for ApproximateTokenCounter {
    fn estimator_identity(&self) -> Option<String> {
        Some(format!(
            "approximate-v1:{}:{}",
            self.count_bytes, self.units_per_token
        ))
    }

    fn count_tokens(&self, request: &ModelRequest) -> usize {
        self.count_request_prefix(request, request.messages.len())
    }

    fn count_prefix_tokens(&self, request: &ModelRequest, message_count: usize) -> usize {
        self.count_request_prefix(request, message_count)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproximateTokenCounterError {
    ZeroCharsPerToken,
}

impl fmt::Display for ApproximateTokenCounterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCharsPerToken => {
                write!(formatter, "characters per token must be greater than zero")
            }
        }
    }
}

impl Error for ApproximateTokenCounterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelMessage, ProviderContextIdentity, ProviderContextWindow};

    fn request(content: &str) -> ModelRequest {
        ModelRequest {
            instructions: String::new(),
            messages: vec![ModelMessage::user(content.into())],
            tools: Vec::new(),
            max_output_tokens: None,
            provider_context: None,
        }
    }

    #[test]
    fn default_uses_utf8_bytes_and_explicit_ratio_keeps_characters() {
        let input = request("你🙂");
        assert_eq!(ApproximateTokenCounter::default().count_tokens(&input), 26);
        assert_eq!(
            ApproximateTokenCounter::conservative().count_tokens(&input),
            31
        );
        assert_eq!(
            ApproximateTokenCounter::new(1)
                .unwrap()
                .count_tokens(&input),
            2
        );
    }

    #[test]
    fn opaque_tokens_are_exact_and_ciphertext_length_is_ignored() {
        let counter = ApproximateTokenCounter::default();
        let mut input = request("hello");
        let before = counter.count_tokens(&input);
        input.provider_context = Some(ProviderContextWindow {
            prefix_fingerprint: None,
            identity: ProviderContextIdentity {
                provider: "provider".into(),
                model: "model".into(),
                format: "v1".into(),
            },
            items: vec![serde_json::json!({"opaque": "x".repeat(100_000)})],
            input_tokens: 321,
        });
        assert_eq!(counter.count_tokens(&input), before + 321);
        input.provider_context.as_mut().unwrap().input_tokens = usize::MAX;
        assert_eq!(counter.count_tokens(&input), usize::MAX);
    }

    #[test]
    fn prefix_count_keeps_fixed_cost_without_counting_tail() {
        let counter = ApproximateTokenCounter::default();
        let mut input = request("hello");
        input.instructions = "instruction".into();
        let before = counter.count_tokens(&input);
        input
            .messages
            .push(ModelMessage::assistant("more".into(), None));
        assert_eq!(counter.count_prefix_tokens(&input, 1), before);
        assert_eq!(
            counter.count_prefix_tokens(&input, usize::MAX),
            counter.count_tokens(&input)
        );
    }
}
