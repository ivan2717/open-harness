use agent_runtime::{
    ContentBlock, ContextCapabilities, Model, ModelBlockKind, ModelCapabilities,
    ModelCapabilitiesError, ModelFinishReason, ModelOutput, ModelRequest, ModelResponse,
    ModelStreamAssembler, ModelStreamError, ModelStreamEvent, ModelStreamSink, ModelUsage,
    ProviderContextIdentity, ProviderContextWindow, ProviderReplayScope, ProviderResponseState,
    ReasoningCapabilities, ReasoningOption,
};
use serde_json::json;
use std::convert::Infallible;

fn finish(reason: ModelFinishReason) -> ModelStreamEvent {
    ModelStreamEvent::Finish {
        finish_reason: reason,
        provider_state: None,
        provider_context: None,
        context_compacted: false,
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn call_block(arguments: &str) -> ContentBlock {
    ContentBlock::ToolCall {
        id: "call-1".into(),
        name: "read".into(),
        arguments: arguments.into(),
    }
}

#[test]
fn interleaved_blocks_settle_in_start_order_and_usage_is_a_snapshot() {
    let mut stream = ModelStreamAssembler::new();
    let reasoning = ContentBlock::Reasoning {
        text: "Check both inputs.".into(),
    };
    let text = text_block("Reading.");
    let call = call_block(r#"{"path":"README.md"}"#);
    for event in [
        ModelStreamEvent::BlockStart {
            index: 0,
            kind: ModelBlockKind::Reasoning,
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            text: "Check ".into(),
        },
        ModelStreamEvent::BlockStart {
            index: 1,
            kind: ModelBlockKind::Text,
        },
        ModelStreamEvent::TextDelta {
            index: 1,
            text: "Reading.".into(),
        },
        ModelStreamEvent::BlockStart {
            index: 2,
            kind: ModelBlockKind::ToolCall,
        },
        ModelStreamEvent::ToolCallDelta {
            index: 2,
            id: Some("call-1".into()),
            name: Some("read".into()),
            arguments_delta: r#"{"path":""#.into(),
        },
        ModelStreamEvent::BlockEnd {
            index: 1,
            block: text.clone(),
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            text: "both inputs.".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 2,
            id: None,
            name: None,
            arguments_delta: r#"README.md"}"#.into(),
        },
        ModelStreamEvent::BlockEnd {
            index: 2,
            block: call.clone(),
        },
        ModelStreamEvent::Usage {
            usage: ModelUsage {
                input_tokens: 100,
                output_tokens: 10,
                cached_input_tokens: None,
            },
        },
        ModelStreamEvent::BlockEnd {
            index: 0,
            block: reasoning.clone(),
        },
        ModelStreamEvent::Usage {
            usage: ModelUsage {
                input_tokens: 100,
                output_tokens: 22,
                cached_input_tokens: Some(80),
            },
        },
        finish(ModelFinishReason::ToolCalls),
    ] {
        stream.emit(event);
    }
    let output = stream.finish().unwrap();
    assert_eq!(output.response.content, [reasoning, text, call]);
    assert_eq!(output.response.finish_reason, ModelFinishReason::ToolCalls);
    assert_eq!(output.usage.unwrap().output_tokens, 22);
}

#[test]
fn incomplete_attempts_cannot_return_completed_or_partial_tool_calls() {
    let mut incomplete = ModelStreamAssembler::new();
    incomplete.emit(ModelStreamEvent::BlockStart {
        index: 0,
        kind: ModelBlockKind::ToolCall,
    });
    incomplete.emit(ModelStreamEvent::ToolCallDelta {
        index: 0,
        id: Some("call-1".into()),
        name: Some("read".into()),
        arguments_delta: "{".into(),
    });
    assert_eq!(
        incomplete.finish().unwrap_err(),
        ModelStreamError::MissingFinish
    );

    let mut open = ModelStreamAssembler::new();
    open.emit(ModelStreamEvent::BlockStart {
        index: 0,
        kind: ModelBlockKind::ToolCall,
    });
    open.emit(finish(ModelFinishReason::ToolCalls));
    assert_eq!(
        open.finish().unwrap_err(),
        ModelStreamError::OpenBlocksAtFinish
    );

    let mut closed_without_finish = ModelStreamAssembler::new();
    closed_without_finish.emit(ModelStreamEvent::BlockStart {
        index: 0,
        kind: ModelBlockKind::ToolCall,
    });
    closed_without_finish.emit(ModelStreamEvent::BlockEnd {
        index: 0,
        block: call_block("{}"),
    });
    assert_eq!(
        closed_without_finish.finish().unwrap_err(),
        ModelStreamError::MissingFinish
    );
}

#[test]
fn a_settled_block_cannot_silently_replace_streamed_content_or_call_identity() {
    for (delta, block) in [
        (
            ModelStreamEvent::TextDelta {
                index: 0,
                text: "secret streamed text".into(),
            },
            text_block("replacement"),
        ),
        (
            ModelStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("different-id".into()),
                name: Some("read".into()),
                arguments_delta: "{}".into(),
            },
            call_block("{}"),
        ),
        (
            ModelStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("read".into()),
                arguments_delta: "{\"secret\":\"original\"}".into(),
            },
            call_block("{}"),
        ),
    ] {
        let kind = if matches!(block, ContentBlock::Text { .. }) {
            ModelBlockKind::Text
        } else {
            ModelBlockKind::ToolCall
        };
        let mut stream = ModelStreamAssembler::new();
        stream.emit(ModelStreamEvent::BlockStart { index: 0, kind });
        stream.emit(delta);
        stream.emit(ModelStreamEvent::BlockEnd { index: 0, block });
        stream.emit(finish(ModelFinishReason::Stop));
        let error = stream.finish().unwrap_err();
        assert_eq!(error, ModelStreamError::BlockContentMismatch { index: 0 });
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
}

#[test]
fn malformed_event_order_and_post_completion_events_are_rejected() {
    let cases = [
        (
            vec![ModelStreamEvent::BlockStart {
                index: 2,
                kind: ModelBlockKind::Text,
            }],
            ModelStreamError::UnexpectedBlockIndex {
                expected: 0,
                actual: 2,
            },
        ),
        (
            vec![ModelStreamEvent::TextDelta {
                index: 0,
                text: "a".into(),
            }],
            ModelStreamError::UnknownBlock { index: 0 },
        ),
        (
            vec![
                ModelStreamEvent::BlockStart {
                    index: 0,
                    kind: ModelBlockKind::Reasoning,
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    text: "a".into(),
                },
            ],
            ModelStreamError::BlockKindMismatch { index: 0 },
        ),
        (
            vec![
                ModelStreamEvent::BlockStart {
                    index: 0,
                    kind: ModelBlockKind::Text,
                },
                ModelStreamEvent::BlockEnd {
                    index: 0,
                    block: text_block("a"),
                },
                ModelStreamEvent::TextDelta {
                    index: 0,
                    text: "b".into(),
                },
            ],
            ModelStreamError::BlockAlreadyClosed { index: 0 },
        ),
        (
            vec![
                finish(ModelFinishReason::Stop),
                finish(ModelFinishReason::Stop),
            ],
            ModelStreamError::EventAfterFinish,
        ),
        (
            vec![ModelStreamEvent::Usage {
                usage: ModelUsage {
                    input_tokens: 2,
                    output_tokens: 1,
                    cached_input_tokens: Some(3),
                },
            }],
            ModelStreamError::InvalidUsage,
        ),
    ];
    for (events, expected) in cases {
        let mut stream = ModelStreamAssembler::new();
        for event in events {
            stream.emit(event);
        }
        assert_eq!(stream.finish().unwrap_err(), expected);
    }
}

#[test]
fn opaque_replay_state_and_native_context_survive_stream_settlement() {
    let identity = ProviderContextIdentity {
        provider: "example".into(),
        model: "model".into(),
        format: "adapter-v1".into(),
    };
    let state = ProviderResponseState::new(
        identity.clone(),
        "custom-protocol",
        ProviderReplayScope::Prefix,
        json!({"signed": "opaque+/==", "unknown": [1, 2, 3]}),
        27,
    )
    .unwrap();
    let context = ProviderContextWindow {
        identity,
        items: vec![json!({"native": "opaque-state"})],
        input_tokens: 45,
        prefix_fingerprint: Some("bound-prefix".into()),
    };
    let expected = ModelOutput {
        response: ModelResponse {
            content: vec![
                ContentBlock::Reasoning {
                    text: "Visible summary".into(),
                },
                text_block("Done"),
            ],
            finish_reason: ModelFinishReason::Stop,
        },
        usage: None,
        provider_state: Some(state),
        provider_context: Some(context),
        context_compacted: true,
    };
    let mut stream = ModelStreamAssembler::new();
    expected.clone().emit_stream(&mut stream);
    assert_eq!(stream.finish().unwrap(), expected);
}

struct NonStreamingModel;

impl Model for NonStreamingModel {
    type Error = Infallible;

    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(ModelResponse {
            content: vec![text_block("One complete response")],
            finish_reason: ModelFinishReason::Stop,
        })
    }
}

#[tokio::test]
async fn non_streaming_adapters_use_the_same_settlement_contract() {
    let request = ModelRequest {
        instructions: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        max_output_tokens: None,
        provider_context: None,
    };
    let mut stream = ModelStreamAssembler::new();
    NonStreamingModel
        .stream(request, &mut stream)
        .await
        .unwrap();
    assert_eq!(
        stream.finish().unwrap().response.content,
        [text_block("One complete response")]
    );
}

#[test]
fn reasoning_choices_are_adapter_defined_and_validated_without_wire_names() {
    let capabilities = ModelCapabilities {
        reasoning: ReasoningCapabilities {
            options: vec![
                ReasoningOption {
                    id: "balanced".into(),
                    label: "Balanced".into(),
                },
                ReasoningOption {
                    id: "extended".into(),
                    label: "Extended".into(),
                },
            ],
            default_option: Some("balanced".into()),
        },
        context: ContextCapabilities::default(),
    };
    capabilities.validate_reasoning_option(None).unwrap();
    capabilities
        .validate_reasoning_option(Some("extended"))
        .unwrap();
    assert_eq!(
        capabilities
            .validate_reasoning_option(Some("high"))
            .unwrap_err(),
        ModelCapabilitiesError::UnsupportedReasoningOption,
    );
    let mut duplicate = capabilities.clone();
    duplicate
        .reasoning
        .options
        .push(duplicate.reasoning.options[0].clone());
    assert_eq!(
        duplicate.validate().unwrap_err(),
        ModelCapabilitiesError::DuplicateReasoningOption
    );
    let mut unknown = capabilities;
    unknown.reasoning.default_option = Some("missing".into());
    assert_eq!(
        unknown.validate().unwrap_err(),
        ModelCapabilitiesError::UnknownDefaultReasoningOption
    );
}

#[test]
fn native_compaction_capability_requires_native_context_support() {
    let mut capabilities = ModelCapabilities::default();
    capabilities.context.supports_native_compaction = true;
    assert_eq!(
        capabilities.validate().unwrap_err(),
        ModelCapabilitiesError::CompactionWithoutNativeContext
    );
    capabilities.context.accepts_native_context = true;
    capabilities.validate().unwrap();
}
