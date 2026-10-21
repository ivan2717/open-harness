//! Provider-neutral conversation values shared by persistence and adapters.

use serde::{Deserialize, Serialize};

use crate::provider_state::{ProviderResponseState, ProviderToolCall};

/// Ordered model-visible content. Provider signatures and encrypted state live
/// on the containing message, never in display text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
}

impl From<ProviderToolCall> for ContentBlock {
    fn from(call: ProviderToolCall) -> Self {
        Self::ToolCall {
            id: call.id,
            name: call.name,
            arguments: call.arguments,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

/// Provenance of runtime-generated context. Adapters must keep these messages
/// at user authority; a summary is historical context, not a new instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageContext {
    Protected,
    Summary { checkpoint_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state: Option<ProviderResponseState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
}

impl ModelMessage {
    pub fn validate(&self) -> Result<(), ModelProtocolError> {
        if self.provider_state.is_some() && self.role != ModelRole::Assistant {
            return Err(ModelProtocolError::MisplacedProviderState);
        }
        if self.context.is_some() && self.role != ModelRole::User {
            return Err(ModelProtocolError::MisplacedContext);
        }
        for block in &self.content {
            match block {
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. }
                    if self.role != ModelRole::Assistant =>
                {
                    return Err(ModelProtocolError::InvalidBlockRole);
                }
                ContentBlock::ToolResult { call_id, .. } => {
                    if self.role != ModelRole::Tool || call_id.trim().is_empty() {
                        return Err(ModelProtocolError::InvalidBlockRole);
                    }
                }
                _ if self.role == ModelRole::Tool => {
                    return Err(ModelProtocolError::InvalidBlockRole);
                }
                _ => {}
            }
            validate_call(block)?;
        }
        Ok(())
    }
    pub fn user(text: String) -> Self {
        Self::text_message(ModelRole::User, text)
    }

    pub fn assistant(text: String, provider_state: Option<ProviderResponseState>) -> Self {
        Self::assistant_blocks(vec![ContentBlock::Text { text }], provider_state)
    }

    pub fn assistant_blocks(
        content: Vec<ContentBlock>,
        provider_state: Option<ProviderResponseState>,
    ) -> Self {
        Self {
            role: ModelRole::Assistant,
            content,
            provider_state,
            context: None,
        }
    }

    pub fn tool_call(call: ProviderToolCall) -> Self {
        Self::assistant_blocks(vec![call.into()], None)
    }

    pub fn tool_result(call_id: String, content: String, is_error: bool) -> Self {
        Self {
            role: ModelRole::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            }],
            provider_state: None,
            context: None,
        }
    }

    pub fn protected(text: String) -> Self {
        Self {
            context: Some(MessageContext::Protected),
            ..Self::user(text)
        }
    }

    pub fn summary(checkpoint_id: String, text: String) -> Self {
        Self {
            context: Some(MessageContext::Summary { checkpoint_id }),
            ..Self::user(text)
        }
    }

    fn text_message(role: ModelRole, text: String) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text }],
            provider_state: None,
            context: None,
        }
    }

    pub fn text(&self) -> String {
        visible_text(&self.content)
    }

    pub fn tool_calls(&self) -> Vec<ProviderToolCall> {
        tool_calls(&self.content)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFinishReason {
    Stop,
    ToolCalls,
    InputRequired,
    /// The output budget was exhausted. The runtime must not execute calls
    /// from this response even if a partial tool block happens to parse.
    Length,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub content: Vec<ContentBlock>,
    pub finish_reason: ModelFinishReason,
}

impl ModelResponse {
    pub fn validate(&self) -> Result<(), ModelProtocolError> {
        for block in &self.content {
            if matches!(block, ContentBlock::ToolResult { .. }) {
                return Err(ModelProtocolError::InvalidBlockRole);
            }
            validate_call(block)?;
        }
        if !self.calls().is_empty()
            && matches!(
                self.finish_reason,
                ModelFinishReason::Stop | ModelFinishReason::InputRequired
            )
        {
            return Err(ModelProtocolError::ConflictingFinishReason);
        }
        Ok(())
    }
    pub fn final_answer(answer: String) -> Self {
        Self {
            content: vec![ContentBlock::Text { text: answer }],
            finish_reason: ModelFinishReason::Stop,
        }
    }

    pub fn tool_calls(text: String, calls: Vec<ProviderToolCall>) -> Self {
        Self {
            content: std::iter::once(ContentBlock::Text { text })
                .chain(calls.into_iter().map(ContentBlock::from))
                .collect(),
            finish_reason: ModelFinishReason::ToolCalls,
        }
    }

    pub fn input_required(prompt: String) -> Self {
        Self {
            finish_reason: ModelFinishReason::InputRequired,
            ..Self::final_answer(prompt)
        }
    }

    pub fn text(&self) -> String {
        visible_text(&self.content)
    }

    pub fn calls(&self) -> Vec<ProviderToolCall> {
        tool_calls(&self.content)
    }

    /// Projection for summaries and history tools. Opaque provider state is
    /// removed separately; reasoning must not leak through canonical blocks.
    pub fn without_reasoning(&self) -> Self {
        Self {
            content: self
                .content
                .iter()
                .filter(|block| !matches!(block, ContentBlock::Reasoning { .. }))
                .cloned()
                .collect(),
            finish_reason: self.finish_reason,
        }
    }
}

fn visible_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_calls(content: &[ContentBlock]) -> Vec<ProviderToolCall> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some(ProviderToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn validate_call(block: &ContentBlock) -> Result<(), ModelProtocolError> {
    if let ContentBlock::ToolCall { id, name, .. } = block
        && (id.trim().is_empty() || name.trim().is_empty())
    {
        return Err(ModelProtocolError::InvalidToolCall);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProtocolError {
    MisplacedProviderState,
    MisplacedContext,
    InvalidBlockRole,
    InvalidToolCall,
    ConflictingFinishReason,
}

impl std::fmt::Display for ModelProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MisplacedProviderState => {
                "provider replay state must belong to an assistant message"
            }
            Self::MisplacedContext => "historical context must retain user authority",
            Self::InvalidBlockRole => "content block is not valid for its message role",
            Self::InvalidToolCall => "tool call must have a nonempty id and name",
            Self::ConflictingFinishReason => "tool calls conflict with the response finish reason",
        })
    }
}
impl std::error::Error for ModelProtocolError {}
