use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use agent_runtime::{
    ContentBlock, ContextCapabilities, Model, ModelCapabilities, ModelContextMetadata,
    ModelFinishReason, ModelMessage, ModelOutput, ModelProtocolError, ModelRequest, ModelResponse,
    ModelStreamAssembler, ModelStreamError, ModelStreamEvent, ModelStreamSink, ModelUsage,
    ProviderContextIdentity, ProviderContextWindow, ProviderReplayScope, ProviderResponseState,
    ProviderStateError, ReasoningCapabilities, ReasoningOption, ReplayRequirement, StreamingModel,
    StreamingModelAdapter, StreamingModelError,
};
use serde_json::json;

fn request() -> ModelRequest {
    ModelRequest {
        instructions: "Follow the task".into(),
        messages: vec![ModelMessage::user("Inspect".into())],
        tools: vec![],
        max_output_tokens: Some(100),
        provider_context: None,
    }
}

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "custom-stream".into(),
        model: "stream-model".into(),
        format: "custom-v1".into(),
    }
}

fn output() -> ModelOutput {
    ModelOutput {
        response: ModelResponse {
            content: vec![
                ContentBlock::Reasoning {
                    text: "Compare evidence".into(),
                },
                ContentBlock::Text {
                    text: "Done".into(),
                },
            ],
            finish_reason: ModelFinishReason::Stop,
        },
        provider_state: Some(
            ProviderResponseState::new(
                identity(),
                "custom-format",
                ProviderReplayScope::Response,
                json!({"opaque":"state"}),
                12,
            )
            .unwrap(),
        ),
        usage: Some(ModelUsage {
            input_tokens: 50,
            output_tokens: 20,
            cached_input_tokens: Some(30),
        }),
        provider_context: Some(ProviderContextWindow {
            identity: identity(),
            items: vec![json!({"native":"window"})],
            input_tokens: 60,
            prefix_fingerprint: None,
        }),
        context_compacted: true,
    }
}

struct StreamingOnly {
    metadata: ModelContextMetadata,
    identity: ProviderContextIdentity,
    calls: AtomicUsize,
    validations: AtomicUsize,
}

impl StreamingOnly {
    fn new() -> Self {
        Self {
            metadata: ModelContextMetadata::new("custom-stream", "stream-model", 128_000).unwrap(),
            identity: identity(),
            calls: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        }
    }
}

impl StreamingModel for StreamingOnly {
    type Error = io::Error;

    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> io::Result<()> {
        assert_eq!(request.messages[0].text(), "Inspect");
        self.calls.fetch_add(1, Ordering::SeqCst);
        output().emit_stream(sink);
        Ok(())
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            reasoning: ReasoningCapabilities {
                options: vec![ReasoningOption {
                    id: "careful".into(),
                    label: "Careful".into(),
                }],
                default_option: Some("careful".into()),
            },
            context: ContextCapabilities {
                accepts_native_context: true,
                supports_native_compaction: true,
                replay_requirement: ReplayRequirement::Required,
            },
        }
    }

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }
    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        error.kind() == io::ErrorKind::InvalidInput
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    fn requires_provider_state(&self) -> bool {
        true
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        if state.payload == json!({"opaque":"state"}) && content == output().response.content {
            Ok(())
        } else {
            Err(ProviderStateError::ContentMismatch)
        }
    }
}

#[tokio::test]
async fn streaming_only_adapter_provides_both_settled_apis_and_live_stream_without_recursion() {
    let adapter = StreamingModelAdapter::new(StreamingOnly::new());
    assert_eq!(
        adapter.respond_with_metadata(request()).await.unwrap(),
        output()
    );
    assert_eq!(adapter.respond(request()).await.unwrap(), output().response);
    let mut assembler = ModelStreamAssembler::new();
    adapter.stream(request(), &mut assembler).await.unwrap();
    assert_eq!(assembler.finish().unwrap(), output());
    assert_eq!(
        adapter.inner().calls.load(Ordering::SeqCst),
        3,
        "each API must dispatch exactly one stream"
    );
    assert_eq!(adapter.into_inner().calls.load(Ordering::SeqCst), 3);
}

#[test]
fn adapter_forwards_capabilities_identity_metadata_and_private_state_validation() {
    let adapter = StreamingModelAdapter::new(StreamingOnly::new());
    assert_eq!(adapter.capabilities(), adapter.inner().capabilities());
    assert_eq!(adapter.context_metadata(), Some(&adapter.inner().metadata));
    assert!(
        adapter.is_context_overflow(&StreamingModelError::Transport(io::Error::new(
            io::ErrorKind::InvalidInput,
            "provider context overflow",
        )))
    );
    assert!(
        !adapter.is_context_overflow(&StreamingModelError::Transport(io::Error::other(
            "network failure",
        )))
    );
    assert!(!adapter.is_context_overflow(&StreamingModelError::Stream(
        ModelStreamError::MissingFinish,
    )));
    assert!(!adapter.is_context_overflow(&StreamingModelError::Protocol(
        ModelProtocolError::ConflictingFinishReason,
    )));
    assert_eq!(adapter.provider_context_identity(), Some(&identity()));
    assert!(adapter.requires_provider_state());
    let output = output();
    adapter
        .validate_replay_state(
            output.provider_state.as_ref().unwrap(),
            &output.response.content,
        )
        .unwrap();
    assert_eq!(
        adapter.validate_replay_state(output.provider_state.as_ref().unwrap(), &[]),
        Err(ProviderStateError::ContentMismatch)
    );
    assert_eq!(adapter.inner().validations.load(Ordering::SeqCst), 2);
}

#[derive(Clone, Copy)]
enum Failure {
    TransportReset,
    EmptyStream,
    InvalidStop,
}

impl StreamingModel for Failure {
    type Error = io::Error;
    async fn stream(&self, _: ModelRequest, sink: &mut dyn ModelStreamSink) -> io::Result<()> {
        match self {
            Self::TransportReset => {
                ModelOutput::from(ModelResponse::final_answer("uncommitted".into()))
                    .emit_stream(sink);
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "connection reset",
                ))
            }
            Self::EmptyStream => Ok(()),
            Self::InvalidStop => {
                ModelOutput::from(ModelResponse {
                    content: vec![ContentBlock::ToolCall {
                        id: "call".into(),
                        name: "write".into(),
                        arguments: "{}".into(),
                    }],
                    finish_reason: ModelFinishReason::Stop,
                })
                .emit_stream(sink);
                Ok(())
            }
        }
    }
}

#[tokio::test]
async fn settled_apis_return_typed_transport_stream_and_canonical_protocol_errors() {
    let transport = StreamingModelAdapter::new(Failure::TransportReset)
        .respond_with_metadata(request())
        .await
        .unwrap_err();
    assert!(
        matches!(&transport, StreamingModelError::Transport(error) if error.kind() == io::ErrorKind::ConnectionReset)
    );
    assert!(std::error::Error::source(&transport).is_some());
    assert!(matches!(
        StreamingModelAdapter::new(Failure::EmptyStream)
            .respond(request())
            .await,
        Err(StreamingModelError::Stream(ModelStreamError::MissingFinish))
    ));
    assert!(matches!(
        StreamingModelAdapter::new(Failure::InvalidStop)
            .respond_with_metadata(request())
            .await,
        Err(StreamingModelError::Protocol(
            ModelProtocolError::ConflictingFinishReason
        ))
    ));
}

#[test]
fn a_minimal_stream_implementation_gets_safe_optional_capability_defaults() {
    let adapter = StreamingModelAdapter::new(Failure::EmptyStream);
    assert_eq!(adapter.capabilities(), ModelCapabilities::default());
    assert_eq!(adapter.context_metadata(), None);
    assert!(
        !adapter.is_context_overflow(&StreamingModelError::Transport(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unclassified error",
        )))
    );
    assert_eq!(adapter.provider_context_identity(), None);
    assert!(!adapter.requires_provider_state());
    let output = output();
    assert_eq!(
        adapter.validate_replay_state(
            output.provider_state.as_ref().unwrap(),
            &output.response.content
        ),
        Err(ProviderStateError::UnsupportedFormat)
    );
}

struct PendingStream {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicUsize>,
}

struct DropGuard(Arc<AtomicUsize>);
impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl StreamingModel for PendingStream {
    type Error = io::Error;
    async fn stream(&self, _: ModelRequest, sink: &mut dyn ModelStreamSink) -> io::Result<()> {
        let _guard = DropGuard(Arc::clone(&self.dropped));
        sink.emit(ModelStreamEvent::BlockStart {
            index: 0,
            kind: agent_runtime::ModelBlockKind::Reasoning,
        });
        sink.emit(ModelStreamEvent::ReasoningDelta {
            index: 0,
            text: "Partial".into(),
        });
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelling_a_settled_call_drops_the_stream_and_its_uncommitted_assembler() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let adapter = StreamingModelAdapter::new(PendingStream {
        entered: Arc::clone(&entered),
        dropped: Arc::clone(&dropped),
    });
    let task = tokio::spawn(async move { adapter.respond_with_metadata(request()).await });
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}
