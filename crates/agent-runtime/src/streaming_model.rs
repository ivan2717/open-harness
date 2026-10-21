//! An opt-in bridge for adapters whose only transport operation is a stream.
//! Unlike mutually recursive defaults, this requires a concrete stream and
//! provides the settled-response APIs without unreachable implementations.

use std::{error::Error, fmt, future::Future};

use crate::{
    ContentBlock, ContextCapabilities, Model, ModelCapabilities, ModelContextMetadata, ModelOutput,
    ModelProtocolError, ModelRequest, ModelResponse, ModelStreamAssembler, ModelStreamError,
    ModelStreamSink, ProviderContextIdentity, ProviderResponseState, ProviderStateError,
    ReplayRequirement,
};

pub trait StreamingModel: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    /// Emit one request attempt in order. Returning an error never settles the
    /// response; cancellation drops this future and any uncommitted blocks.
    fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

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

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        None
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        None
    }

    fn requires_provider_state(&self) -> bool {
        false
    }

    fn validate_replay_state(
        &self,
        _state: &ProviderResponseState,
        _content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        Err(ProviderStateError::UnsupportedFormat)
    }
}

#[derive(Debug, Clone)]
pub struct StreamingModelAdapter<T> {
    inner: T,
}

impl<T> StreamingModelAdapter<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: StreamingModel> Model for StreamingModelAdapter<T> {
    type Error = StreamingModelError<T::Error>;

    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        self.inner.context_metadata()
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        self.inner.provider_context_identity()
    }

    fn requires_provider_state(&self) -> bool {
        self.inner.requires_provider_state()
    }

    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        self.inner.validate_replay_state(state, content)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        self.inner
            .stream(request, sink)
            .await
            .map_err(StreamingModelError::Transport)
    }

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(self.respond_with_metadata(request).await?.response)
    }

    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        let mut assembler = ModelStreamAssembler::new();
        self.inner
            .stream(request, &mut assembler)
            .await
            .map_err(StreamingModelError::Transport)?;
        let output = assembler.finish().map_err(StreamingModelError::Stream)?;
        output
            .response
            .validate()
            .map_err(StreamingModelError::Protocol)?;
        Ok(output)
    }
}

#[derive(Debug)]
pub enum StreamingModelError<E> {
    Transport(E),
    Stream(ModelStreamError),
    Protocol(ModelProtocolError),
}

impl<E: fmt::Display> fmt::Display for StreamingModelError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "model stream transport failed: {error}"),
            Self::Stream(error) => write!(f, "model stream did not settle: {error}"),
            Self::Protocol(error) => write!(f, "invalid canonical model response: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for StreamingModelError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::Protocol(error) => Some(error),
        }
    }
}
