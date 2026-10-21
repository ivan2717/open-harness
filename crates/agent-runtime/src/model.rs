use crate::{
    ContentBlock, ContextCapabilities, ModelCapabilities, ModelStreamSink, ProviderReplayError,
    ProviderResponseState, ProviderStateError, ReplayRequirement,
};
pub use agent_core::model_protocol::{ModelMessage, ModelResponse};
pub use agent_core::provider_state::{ProviderContextIdentity, ProviderContextWindow};
use std::{error::Error, future::Future};

use crate::{ModelContextMetadata, ToolDefinition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub instructions: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDefinition>,
    /// Maximum generated tokens. Providers must honor this when present.
    pub max_output_tokens: Option<usize>,
    /// Canonical provider-native conversation items preceding `messages`.
    /// Adapters must preserve every item and its order, rather than extracting
    /// only a compaction blob or converting opaque state to ordinary text.
    pub provider_context: Option<ProviderContextWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Complete input cost of this request, including cached input. This is
    /// neither a session cumulative total nor the uncached billing portion.
    pub input_tokens: usize,
    /// Generated tokens reported for this response, including reasoning when
    /// the provider includes it in its output usage.
    pub output_tokens: usize,
    /// A subset of input_tokens; cached input still occupies context capacity.
    pub cached_input_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOutput {
    /// Complete replayable response state, durably committed before tool effects.
    pub provider_state: Option<ProviderResponseState>,
    pub response: ModelResponse,
    pub usage: Option<ModelUsage>,
    /// When provided, the complete conversation window including the request
    /// and this response, excluding future tool results. Do not return only
    /// response.output: the next request replaces its prior window with this.
    pub provider_context: Option<ProviderContextWindow>,
    /// The provider compacted the returned window during this response. Context
    /// managers must restore protected user context before the next request.
    pub context_compacted: bool,
}

impl From<ModelResponse> for ModelOutput {
    fn from(response: ModelResponse) -> Self {
        Self {
            provider_state: None,
            response,
            usage: None,
            provider_context: None,
            context_compacted: false,
        }
    }
}

pub trait Model: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Core decisions use capabilities, never provider wire-protocol names.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            context: ContextCapabilities {
                accepts_native_context: self.provider_context_identity().is_some(),
                replay_requirement: if self.requires_provider_state() {
                    ReplayRequirement::Required
                } else {
                    ReplayRequirement::Optional
                },
                ..ContextCapabilities::default()
            },
            ..ModelCapabilities::default()
        }
    }

    /// The owning adapter validates its private payload against canonical
    /// blocks. Opaque state is rejected unless the adapter implements this.
    fn validate_replay_state(
        &self,
        _state: &ProviderResponseState,
        _content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        Err(ProviderStateError::UnsupportedFormat)
    }

    fn validate_request(&self, request: &ModelRequest) -> Result<(), ProviderReplayError> {
        request.validate_provider_state_with(
            self.provider_context_identity(),
            self.capabilities().context.replay_requirement == ReplayRequirement::Required,
            |state, content| self.validate_replay_state(state, content),
        )
    }

    /// Streaming adapters override this method. Existing settled-response
    /// adapters produce the same block/usage/finish contract through this bridge.
    fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async move {
            self.respond_with_metadata(request).await?.emit_stream(sink);
            Ok(())
        }
    }

    /// Advertises the configured model's context capabilities. Adapters without
    /// metadata retain the context manager's explicit settings or fallback.
    /// Usage calibration requires this stable identity to prevent observations
    /// from a different model from being reused for the same run.
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        None
    }

    /// Opts into receiving native provider windows with this exact identity.
    /// The runtime rejects incompatible or unsupported opaque state.
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        None
    }

    /// Requires durable replay data for every assistant response, including
    /// legacy histories. Reasoning adapters should opt in; missing state fails
    /// before model calls or tool effects instead of silently losing continuity.
    fn requires_provider_state(&self) -> bool {
        false
    }

    /// The runtime may drop this future when execution is cancelled. Providers
    /// should release owned requests/resources on drop rather than detach work.
    fn respond(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, Self::Error>> + Send;

    /// Providers that support native continuity or usage reporting override
    /// this method. Existing text-only adapters need only implement `respond`.
    fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelOutput, Self::Error>> + Send {
        async move { self.respond(request).await.map(ModelOutput::from) }
    }
}
