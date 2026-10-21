//! Bounded model context, derived checkpoints, and compaction adapters.
mod budget;
mod checkpoint;
mod compactor;
mod error;
mod manager;
mod native;
mod policy;
mod projection;
mod tokens;
mod usage;

pub use budget::{ModelContextBudget, ModelContextBudgetError};
pub use checkpoint::{
    ContextCheckpoint, ContextCheckpointStore, InMemoryContextCheckpointStore,
    InMemoryContextCheckpointStoreError,
};
pub use compactor::{
    ContextCompactor, ContextSummary, ModelContextCompactor, ModelContextCompactorError,
};
pub use error::CompactingContextError;
pub use manager::CompactingContextManager;
pub use native::{
    InMemoryNativeContextCheckpointStore, InMemoryNativeContextCheckpointStoreError,
    NativeContextCheckpoint, NativeContextCheckpointStore, NativeContextCompactor,
    NativeContextError, NativeContextManager,
};
pub use policy::{ContextPolicy, ContextPolicyBuilder, ContextPolicyError};
pub use tokens::{ApproximateTokenCounter, ApproximateTokenCounterError, TokenCounter};

use crate::{ModelContextMetadata, ModelOutput, ModelRequest, ToolDefinition, ToolOutputProjector};
use agent_core::agent::RunEventEnvelope;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, future::Future};

/// Owned event and tool data for preparing one model request.
///
/// `deferred_tools` are capabilities, such as cold-history lookup, that are only
/// useful after a summary or clipped payload has omitted details from the model
/// context. They are deliberately excluded until the active projection omits
/// information that the model may need to recover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInput {
    pub run_id: String,
    pub instructions: String,
    pub events: Vec<RunEventEnvelope>,
    pub tools: Vec<ToolDefinition>,
    pub deferred_tools: Vec<ToolDefinition>,
    /// Capabilities of the model receiving this request, independent of the
    /// model used to summarize history.
    pub model_metadata: Option<ModelContextMetadata>,
}

impl ContextInput {
    pub fn new(
        run_id: impl Into<String>,
        instructions: impl Into<String>,
        events: Vec<RunEventEnvelope>,
        tools: Vec<ToolDefinition>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            instructions: instructions.into(),
            events,
            tools,
            deferred_tools: Vec::new(),
            model_metadata: None,
        }
    }

    pub fn with_deferred_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.deferred_tools = tools;
        self
    }

    pub fn with_model_metadata(mut self, metadata: ModelContextMetadata) -> Self {
        self.model_metadata = Some(metadata);
        self
    }

    pub fn as_ref(&self) -> ContextInputRef<'_> {
        ContextInputRef {
            run_id: &self.run_id,
            instructions: &self.instructions,
            events: &self.events,
            tools: &self.tools,
            deferred_tools: &self.deferred_tools,
            tool_output_projector: None,
            model_metadata: self.model_metadata.as_ref(),
        }
    }
}

/// A borrowed snapshot for context managers that project only an active tail.
/// This avoids cloning the entire durable history before selecting that tail.
#[derive(Clone, Copy)]
pub struct ContextInputRef<'a> {
    pub run_id: &'a str,
    pub instructions: &'a str,
    pub events: &'a [RunEventEnvelope],
    pub tools: &'a [ToolDefinition],
    pub deferred_tools: &'a [ToolDefinition],
    pub tool_output_projector: Option<&'a dyn ToolOutputProjector>,
    pub model_metadata: Option<&'a ModelContextMetadata>,
}

impl fmt::Debug for ContextInputRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextInputRef")
            .field("run_id", &self.run_id)
            .field("instructions", &self.instructions)
            .field("events", &self.events)
            .field("tools", &self.tools)
            .field("deferred_tools", &self.deferred_tools)
            .field("model_metadata", &self.model_metadata)
            .field(
                "has_tool_output_projector",
                &self.tool_output_projector.is_some(),
            )
            .finish()
    }
}

impl<'a> ContextInputRef<'a> {
    pub fn with_tool_output_projector(mut self, projector: &'a dyn ToolOutputProjector) -> Self {
        self.tool_output_projector = Some(projector);
        self
    }

    /// Copies the data into an owned snapshot. Attach any output projector again
    /// when borrowing it; projector references are not stored in owned snapshots.
    pub fn into_owned(self) -> ContextInput {
        ContextInput {
            run_id: self.run_id.to_owned(),
            instructions: self.instructions.to_owned(),
            events: self.events.to_vec(),
            tools: self.tools.to_vec(),
            deferred_tools: self.deferred_tools.to_vec(),
            model_metadata: self.model_metadata.cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedContext {
    pub request: ModelRequest,
    pub omitted_history: Option<OmittedHistory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedHistory {
    /// The last event represented by the checkpoint summary (inclusive).
    /// When there is no checkpoint, this is the last clipped event.
    pub through_sequence: u64,
    /// The last event that the history tools may read. This can extend past
    /// the checkpoint when a recent tool payload was clipped in the tail.
    pub readable_through_sequence: u64,
    pub checkpoint_id: String,
}

/// Converts durable run history into the bounded context sent to a model.
pub trait ContextManager: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// The largest tool payload this manager preserves verbatim in model context.
    /// Structured tools can use this budget while encoding their responses.
    fn max_tool_output_chars(&self) -> Option<usize> {
        None
    }

    /// Estimate for diagnostics. Providers can also report actual usage with
    /// `ModelOutput`; neither value is inferred from opaque ciphertext length.
    fn estimate_request_tokens(&self, _request: &ModelRequest) -> Option<usize> {
        None
    }

    /// Observes a successful response after its text and tool declarations are
    /// durable, before any tool executes. The snapshot excludes future results.
    /// Native strategies can checkpoint provider continuation items here.
    fn observe_response(
        &self,
        _input: ContextInputRef<'_>,
        _output: &ModelOutput,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }

    /// Prepares a model request without copying the durable history up front.
    fn prepare(
        &self,
        input: ContextInputRef<'_>,
    ) -> impl Future<Output = Result<PreparedContext, Self::Error>> + Send;

    /// Convenience for callers that already own a snapshot. Structured output
    /// adapters should use `prepare` and attach their projector to the borrow.
    fn prepare_owned(
        &self,
        input: ContextInput,
    ) -> impl Future<Output = Result<PreparedContext, Self::Error>> + Send {
        async move { self.prepare(input.as_ref()).await }
    }
}
