mod provider_replay;
pub use agent_core::provider_state::{
    ProviderContinuation, ProviderReplayScope, ProviderResponseState, ProviderStateError,
};
pub use provider_replay::ProviderReplayError;
mod context;
mod history;
mod model;
mod model_context;
mod runtime;
mod storage;
mod subscription;
mod tool;

pub use agent_core::token_usage::{ModelUsageRecord, RequestUsageBaseline};
pub use agent_core::{FileReference, TextFileReference, format_user_input};
pub use agent_core::{ModelImage, ToolOutput};
pub use context::{
    ApproximateTokenCounter, ApproximateTokenCounterError, CompactingContextError,
    CompactingContextManager, CompactionPreparation, CompactionPreparationHook, ContextCheckpoint,
    ContextCheckpointStore, ContextCompactor, ContextInput, ContextInputRef, ContextManager,
    ContextMeasurement, ContextMeasurementSource, ContextPolicy, ContextPolicyBuilder,
    ContextPolicyError, ContextPruning, ContextSummary, InMemoryContextCheckpointStore,
    InMemoryContextCheckpointStoreError, InMemoryNativeContextCheckpointStore,
    InMemoryNativeContextCheckpointStoreError, ModelContextBudget, ModelContextBudgetError,
    ModelContextCompactor, ModelContextCompactorError, NativeContextCheckpoint,
    NativeContextCheckpointStore, NativeContextCompactor, NativeContextError, NativeContextManager,
    OmittedHistory, PreparedContext, PrunedToolOutput, TokenCounter, request_usage_baseline,
    request_usage_baseline_with_counter,
};
pub use history::{
    EventStoreHistoryAccess, HistoryAccess, HistoryAccessError, HistoryConfigurationError,
    HistoryContext, HistoryLimits, HistoryLimitsBuilder, NoHistoryAccess, READ_RUN_HISTORY_TOOL,
    SEARCH_RUN_HISTORY_TOOL, ToggleHistoryAccess, ToggleHistoryAccessError,
};
pub use model::{
    Model, ModelMessage, ModelOutput, ModelRequest, ModelResponse, ModelUsage,
    ProviderContextIdentity, ProviderContextWindow,
};
pub use model_context::{ModelContextMetadata, ModelContextMetadataError};
pub use runtime::{AgentRuntime, AgentRuntimeError};
pub use storage::{
    AppendOutcome, EventStore, EventStoreError, InMemoryEventStore, JsonlEventStore, StoredRun,
};
pub use subscription::{RunEventNotification, RunEventReceiver, RunEventSubscriptions};
pub use tool::{ToolCall, ToolDefinition, ToolExecutor, ToolOutputProjector};

mod model_stream;
mod streaming_model;
pub use streaming_model::{StreamingModel, StreamingModelAdapter, StreamingModelError};
mod model_capabilities;
pub use agent_core::model_protocol::{
    ContentBlock, MessageContext, ModelFinishReason, ModelProtocolError, ModelRole,
};
pub use model_capabilities::{
    ContextCapabilities, ModelCapabilities, ModelCapabilitiesError, ReasoningCapabilities,
    ReasoningOption, ReplayRequirement,
};
pub use model_stream::{
    ModelBlockKind, ModelStreamAssembler, ModelStreamError, ModelStreamEvent, ModelStreamSink,
};
