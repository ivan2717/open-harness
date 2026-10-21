//! Provider-neutral streaming and settlement of one model response.
//!
//! Deltas are presentation data. A response becomes replayable only after all
//! blocks close and the adapter emits `Finish`. Dropping the adapter future on
//! cancellation also drops the uncommitted assembler; partial calls never become
//! executable model output.

use std::{error::Error, fmt};

use crate::{
    ContentBlock, ModelFinishReason, ModelOutput, ModelResponse, ModelUsage, ProviderContextWindow,
    ProviderResponseState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelBlockKind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
}

impl ModelBlockKind {
    fn of(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { .. } => Self::Text,
            ContentBlock::Reasoning { .. } => Self::Reasoning,
            ContentBlock::ToolCall { .. } => Self::ToolCall,
            ContentBlock::ToolResult { .. } => Self::ToolResult,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamEvent {
    /// Start indexes are contiguous and define the final content order. Deltas
    /// for different open blocks may be interleaved.
    BlockStart {
        index: usize,
        kind: ModelBlockKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ReasoningDelta {
        index: usize,
        text: String,
    },
    /// `id` and `name`, when present, are complete values, not fragments. Only
    /// `arguments_delta` is incremental; adapters assemble fragmented IDs/names.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// The settled block must agree with any preceding deltas. Adapters may
    /// deliver a complete block without deltas when their API is not streaming.
    BlockEnd {
        index: usize,
        block: ContentBlock,
    },
    /// Cumulative usage for this response, replacing any previous observation.
    /// The adapter normalizes providers that report usage as increments.
    Usage {
        usage: ModelUsage,
    },
    /// The only event allowed to carry replay state. Emit exactly once, after
    /// all blocks have ended. A transport error must not be converted to Finish.
    Finish {
        finish_reason: ModelFinishReason,
        provider_state: Option<Box<ProviderResponseState>>,
        provider_context: Option<Box<ProviderContextWindow>>,
        context_compacted: bool,
    },
}

/// A synchronous sink keeps stream processing ordered and introduces no
/// detached work. Implementations may retain an error and reject settlement.
pub trait ModelStreamSink: Send {
    fn emit(&mut self, event: ModelStreamEvent);
}

#[derive(Default)]
pub struct ModelStreamAssembler {
    blocks: Vec<StreamBlock>,
    usage: Option<ModelUsage>,
    output: Option<ModelOutput>,
    error: Option<ModelStreamError>,
}

struct StreamBlock {
    kind: ModelBlockKind,
    text: String,
    id: Option<String>,
    name: Option<String>,
    has_delta: bool,
    settled: Option<ContentBlock>,
}

impl ModelStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume an attempt. Neither a missing Finish nor a malformed event
    /// sequence can yield an output, even if some blocks completed earlier.
    pub fn finish(self) -> Result<ModelOutput, ModelStreamError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.output.ok_or(ModelStreamError::MissingFinish)
    }

    fn open_block(&mut self, index: usize) -> Result<&mut StreamBlock, ModelStreamError> {
        let block = self
            .blocks
            .get_mut(index)
            .ok_or(ModelStreamError::UnknownBlock { index })?;
        if block.settled.is_some() {
            return Err(ModelStreamError::BlockAlreadyClosed { index });
        }
        Ok(block)
    }

    fn apply(&mut self, event: ModelStreamEvent) -> Result<(), ModelStreamError> {
        if self.output.is_some() {
            return Err(ModelStreamError::EventAfterFinish);
        }
        match event {
            ModelStreamEvent::BlockStart { index, kind } => {
                if index != self.blocks.len() {
                    return Err(ModelStreamError::UnexpectedBlockIndex {
                        expected: self.blocks.len(),
                        actual: index,
                    });
                }
                self.blocks.push(StreamBlock {
                    kind,
                    text: String::new(),
                    id: None,
                    name: None,
                    has_delta: false,
                    settled: None,
                });
            }
            ModelStreamEvent::TextDelta { index, text } => {
                self.append_text(index, ModelBlockKind::Text, text)?;
            }
            ModelStreamEvent::ReasoningDelta { index, text } => {
                self.append_text(index, ModelBlockKind::Reasoning, text)?;
            }
            ModelStreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let block = self.open_block(index)?;
                if block.kind != ModelBlockKind::ToolCall {
                    return Err(ModelStreamError::BlockKindMismatch { index });
                }
                set_tool_field(&mut block.id, id, index)?;
                set_tool_field(&mut block.name, name, index)?;
                block.has_delta = true;
                block.text.push_str(&arguments_delta);
            }
            ModelStreamEvent::BlockEnd {
                index,
                block: settled,
            } => {
                let block = self.open_block(index)?;
                if block.kind != ModelBlockKind::of(&settled) {
                    return Err(ModelStreamError::BlockKindMismatch { index });
                }
                match &settled {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                        if block.has_delta && block.text != *text {
                            return Err(ModelStreamError::BlockContentMismatch { index });
                        }
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        if id.trim().is_empty() || name.trim().is_empty() {
                            return Err(ModelStreamError::IncompleteToolCall { index });
                        }
                        if block.id.as_ref().is_some_and(|value| value != id)
                            || block.name.as_ref().is_some_and(|value| value != name)
                            || (block.has_delta && block.text != *arguments)
                        {
                            return Err(ModelStreamError::BlockContentMismatch { index });
                        }
                    }
                    ContentBlock::ToolResult { .. } => {}
                }
                block.settled = Some(settled);
            }
            ModelStreamEvent::Usage { usage } => {
                if usage
                    .cached_input_tokens
                    .is_some_and(|cached| cached > usage.input_tokens)
                {
                    return Err(ModelStreamError::InvalidUsage);
                }
                self.usage = Some(usage);
            }
            ModelStreamEvent::Finish {
                finish_reason,
                provider_state,
                provider_context,
                context_compacted,
            } => {
                if self.blocks.iter().any(|block| block.settled.is_none()) {
                    return Err(ModelStreamError::OpenBlocksAtFinish);
                }
                let content = self
                    .blocks
                    .iter_mut()
                    .filter_map(|block| block.settled.take())
                    .collect();
                self.output = Some(ModelOutput {
                    response: ModelResponse {
                        content,
                        finish_reason,
                    },
                    usage: self.usage,
                    provider_state: provider_state.map(|state| *state),
                    provider_context: provider_context.map(|context| *context),
                    context_compacted,
                });
            }
        }
        Ok(())
    }

    fn append_text(
        &mut self,
        index: usize,
        kind: ModelBlockKind,
        text: String,
    ) -> Result<(), ModelStreamError> {
        let block = self.open_block(index)?;
        if block.kind != kind {
            return Err(ModelStreamError::BlockKindMismatch { index });
        }
        block.has_delta = true;
        block.text.push_str(&text);
        Ok(())
    }
}

fn set_tool_field(
    field: &mut Option<String>,
    value: Option<String>,
    index: usize,
) -> Result<(), ModelStreamError> {
    if let Some(value) = value {
        if field.as_ref().is_some_and(|previous| previous != &value) {
            return Err(ModelStreamError::BlockContentMismatch { index });
        }
        *field = Some(value);
    }
    Ok(())
}

impl ModelStreamSink for ModelStreamAssembler {
    fn emit(&mut self, event: ModelStreamEvent) {
        if self.error.is_none() {
            self.error = self.apply(event).err();
        }
    }
}

impl ModelOutput {
    /// Adapt a settled non-streaming response to the same contract. This emits
    /// no fabricated incremental chunks; complete blocks are still observable.
    pub fn emit_stream(self, sink: &mut dyn ModelStreamSink) {
        for (index, block) in self.response.content.into_iter().enumerate() {
            sink.emit(ModelStreamEvent::BlockStart {
                index,
                kind: ModelBlockKind::of(&block),
            });
            sink.emit(ModelStreamEvent::BlockEnd { index, block });
        }
        if let Some(usage) = self.usage {
            sink.emit(ModelStreamEvent::Usage { usage });
        }
        sink.emit(ModelStreamEvent::Finish {
            finish_reason: self.response.finish_reason,
            provider_state: self.provider_state.map(Box::new),
            provider_context: self.provider_context.map(Box::new),
            context_compacted: self.context_compacted,
        });
    }
}

/// Errors contain structural information only, never model text, arguments,
/// reasoning, credentials or opaque replay payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamError {
    MissingFinish,
    EventAfterFinish,
    UnexpectedBlockIndex { expected: usize, actual: usize },
    UnknownBlock { index: usize },
    BlockAlreadyClosed { index: usize },
    BlockKindMismatch { index: usize },
    BlockContentMismatch { index: usize },
    IncompleteToolCall { index: usize },
    OpenBlocksAtFinish,
    InvalidUsage,
}

impl fmt::Display for ModelStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFinish => f.write_str("model stream ended without a completed response"),
            Self::EventAfterFinish => f.write_str("model stream emitted an event after completion"),
            Self::UnexpectedBlockIndex { expected, actual } => write!(
                f,
                "model stream started block {actual}, expected {expected}"
            ),
            Self::UnknownBlock { index } => {
                write!(f, "model stream referenced unknown block {index}")
            }
            Self::BlockAlreadyClosed { index } => {
                write!(f, "model stream changed completed block {index}")
            }
            Self::BlockKindMismatch { index } => {
                write!(f, "model stream changed the type of block {index}")
            }
            Self::BlockContentMismatch { index } => write!(
                f,
                "model stream content disagrees with completed block {index}"
            ),
            Self::IncompleteToolCall { index } => write!(
                f,
                "model stream completed an incomplete tool call in block {index}"
            ),
            Self::OpenBlocksAtFinish => {
                f.write_str("model stream completed with unfinished blocks")
            }
            Self::InvalidUsage => {
                f.write_str("model stream cached input exceeds total input tokens")
            }
        }
    }
}

impl Error for ModelStreamError {}
