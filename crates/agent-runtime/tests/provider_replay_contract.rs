use agent_adapters::WireProtocol;
use agent_runtime::{ContentBlock, ModelRole};
use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ApproximateTokenCounter, ContextInputRef, ContextManager,
    Model, ModelMessage, ModelOutput, ModelRequest, ModelResponse, PreparedContext,
    ProviderContextIdentity, ProviderContextWindow, ProviderReplayError, ProviderReplayScope,
    ProviderResponseState, ProviderStateError, TokenCounter, ToolCall, ToolDefinition,
    ToolExecutor,
};
use serde_json::json;

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "provider".into(),
        model: "reasoning-model".into(),
        format: "responses-v1".into(),
    }
}

fn tool() -> ToolDefinition {
    ToolDefinition {
        name: "read".into(),
        description: "Read one file".into(),
        input_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
    }
}

fn calls() -> Vec<ToolCall> {
    vec![
        ToolCall {
            id: "read-1".into(),
            name: "read".into(),
            arguments: r#"{"path":"a"}"#.into(),
        },
        ToolCall {
            id: "read-2".into(),
            name: "read".into(),
            arguments: r#"{"path":"b"}"#.into(),
        },
    ]
}

fn state(content: &str, calls: &[ToolCall], scope: ProviderReplayScope) -> ProviderResponseState {
    let mut payload = vec![
        json!({"type": "reasoning", "encrypted_content": "opaque-state+/="}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": content}]}),
    ];
    payload.extend(calls.iter().map(|call| {
        json!({
            "type": "function_call", "call_id": call.id, "name": call.name,
            "arguments": call.arguments,
        })
    }));
    WireProtocol::Responses
        .capture_state(identity(), scope, json!(payload), 240)
        .unwrap()
}

fn request() -> ModelRequest {
    let calls = calls();
    let mut messages = vec![
        ModelMessage::user("Inspect both files".into()),
        ModelMessage::assistant(
            "Checking the files".into(),
            Some(state(
                "Checking the files",
                &calls,
                ProviderReplayScope::Response,
            )),
        ),
    ];
    messages[1]
        .content
        .extend(calls.into_iter().map(ContentBlock::from));
    messages.extend([
        ModelMessage::tool_result("read-1".into(), "First file".into(), false),
        ModelMessage::tool_result("read-2".into(), "Second file".into(), false),
    ]);
    ModelRequest {
        instructions: "Be precise.".into(),
        messages,
        tools: vec![tool()],
        max_output_tokens: Some(1024),
        provider_context: None,
    }
}

#[test]
fn response_state_and_native_windows_require_the_exact_adapter_identity() {
    let request = request();
    request
        .validate_provider_state_with(
            Some(&identity()),
            true,
            agent_adapters::validate_replay_state,
        )
        .unwrap();
    for change in 0..3 {
        let mut incompatible = identity();
        match change {
            0 => incompatible.provider = "other-provider".into(),
            1 => incompatible.model = "other-model".into(),
            2 => incompatible.format = "responses-v2".into(),
            _ => unreachable!(),
        }
        assert!(matches!(
            request.validate_provider_state_with(
                Some(&incompatible),
                true,
                agent_adapters::validate_replay_state
            ),
            Err(ProviderReplayError::IdentityMismatch)
        ));
        let mut native = request.clone();
        native.messages.clear();
        native.provider_context = Some(ProviderContextWindow {
            identity: identity(),
            items: vec![json!({"opaque": "native"})],
            input_tokens: 123,
            prefix_fingerprint: None,
        });
        assert!(matches!(
            native.validate_provider_state_with(
                Some(&incompatible),
                false,
                agent_adapters::validate_replay_state
            ),
            Err(ProviderReplayError::IdentityMismatch)
        ));
    }
    assert!(matches!(
        request.validate_provider_state_with(None, false, agent_adapters::validate_replay_state),
        Err(ProviderReplayError::IdentityMismatch)
    ));
}

#[test]
fn replay_preserves_visible_text_and_ordered_tool_call_identity_and_arguments() {
    let original = request();
    for change in 0..6 {
        let mut changed = original.clone();
        match change {
            0 => {
                let ContentBlock::Text { text: content } = &mut changed.messages[1].content[0]
                else {
                    unreachable!()
                };
                content.push_str(" edited");
            }
            1 => changed.messages[1].content.swap(1, 2),
            2 => {
                let ContentBlock::ToolCall { id: call_id, .. } =
                    &mut changed.messages[1].content[1]
                else {
                    unreachable!()
                };
                *call_id = "replacement-id".into();
            }
            3 => {
                let ContentBlock::ToolCall {
                    name: tool_name, ..
                } = &mut changed.messages[1].content[1]
                else {
                    unreachable!()
                };
                *tool_name = "write".into();
            }
            4 => {
                let ContentBlock::ToolCall { arguments, .. } = &mut changed.messages[1].content[1]
                else {
                    unreachable!()
                };
                *arguments = r#"{"path":"different"}"#.into();
            }
            5 => {
                changed.messages[1].content.remove(2);
            }
            _ => unreachable!(),
        }
        let error = changed
            .validate_provider_state_with(
                Some(&identity()),
                true,
                agent_adapters::validate_replay_state,
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                ProviderReplayError::InvalidResponse(ProviderStateError::ContentMismatch)
            ) && change == 0
                || matches!(
                    error,
                    ProviderReplayError::InvalidResponse(ProviderStateError::ToolCallsMismatch)
                ) && change != 0
        );
    }
    // Equivalent JSON whitespace does not change the call's meaning.
    let mut equivalent = original;
    let ContentBlock::ToolCall { arguments, .. } = &mut equivalent.messages[1].content[1] else {
        unreachable!()
    };
    *arguments = "{ \"path\" : \"a\" }".into();
    equivalent
        .validate_provider_state_with(
            Some(&identity()),
            true,
            agent_adapters::validate_replay_state,
        )
        .unwrap();
}

struct FixedContext(ModelRequest);

impl ContextManager for FixedContext {
    type Error = Infallible;

    async fn prepare(&self, _: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        Ok(PreparedContext {
            request: self.0.clone(),
            omitted_history: None,
        })
    }
}

struct CapturingModel {
    identity: ProviderContextIdentity,
    output: Mutex<Option<ModelOutput>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl CapturingModel {
    fn new(output: ModelOutput) -> Self {
        Self {
            identity: identity(),
            output: Mutex::new(Some(output)),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Model for CapturingModel {
    type Error = Infallible;

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), agent_runtime::ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }

    fn requires_provider_state(&self) -> bool {
        true
    }

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        unreachable!("metadata is required")
    }

    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .output
            .lock()
            .unwrap()
            .take()
            .expect("unexpected extra model call"))
    }
}

struct HistoricalTools(Vec<ToolDefinition>);

impl ToolExecutor for HistoricalTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.0
    }

    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        panic!("historical tool calls must not execute")
    }
}

fn output(content: &str) -> ModelOutput {
    ModelOutput {
        response: ModelResponse::final_answer(content.into()),
        provider_state: Some(state(content, &[], ProviderReplayScope::Prefix)),
        usage: None,
        provider_context: None,
        context_compacted: false,
    }
}

#[tokio::test]
async fn runtime_bound_prefix_replays_unchanged_and_rejects_edits_before_model_calls() {
    let agent = Agent::new("agent", "Agent", "Replay test", "Be precise.");
    let original = request();
    let mut waiting = output("Which file first?");
    waiting.response = ModelResponse::input_required("Which file first?".into());
    let runtime = AgentRuntime::new(
        CapturingModel::new(waiting),
        HistoricalTools(vec![tool()]),
        FixedContext(original.clone()),
        None,
    );
    let mut run = runtime
        .start_run(&agent, "bind-prefix", "Inspect both files")
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    let bound = run
        .events()
        .iter()
        .find_map(|event| match &event.event {
            RunEvent::ModelResponseReceived {
                provider_state: Some(state),
                ..
            } => Some(state.clone()),
            _ => None,
        })
        .unwrap();
    assert!(bound.prefix_fingerprint.is_some());
    let mut followup = original;
    followup.messages.extend([
        ModelMessage::assistant("Which file first?".into(), Some(bound)),
        ModelMessage::user("Start with a".into()),
    ]);

    for change in 0..5 {
        let mut prepared = followup.clone();
        match change {
            0 => prepared.max_output_tokens = Some(2048),
            1 => prepared.instructions.push_str(" Changed instructions."),
            2 => prepared.tools[0].input_schema = r#"{"type":"object"}"#.into(),
            3 => {
                let ContentBlock::ToolResult {
                    content: output, ..
                } = &mut prepared.messages[2].content[0]
                else {
                    unreachable!()
                };
                *output = "Rewritten tool result".into();
            }
            4 => prepared.messages.insert(
                0,
                ModelMessage::summary("summary".into(), "Summarized history".into()),
            ),
            _ => unreachable!(),
        }
        let runtime = AgentRuntime::new(
            CapturingModel::new(output("Done")),
            HistoricalTools(vec![tool()]),
            FixedContext(prepared),
            None,
        );
        let mut run = runtime
            .start_run(&agent, format!("replay-{change}"), "Continue")
            .await
            .unwrap();
        let result = runtime.run_until_blocked(&agent, &mut run).await;
        if change == 0 {
            assert_eq!(result.unwrap(), RunStatus::Completed);
            assert_eq!(runtime.model().requests.lock().unwrap().len(), 1);
        } else {
            assert!(matches!(
                result,
                Err(AgentRuntimeError::ProviderReplay(
                    ProviderReplayError::PrefixChanged
                ))
            ));
            assert!(runtime.model().requests.lock().unwrap().is_empty());
            assert!(
                !run.events()
                    .iter()
                    .any(|event| matches!(event.event, RunEvent::ModelRequestStarted))
            );
        }
    }
}

#[test]
fn legacy_events_round_trip_without_new_fields_but_required_replay_rejects_missing_state() {
    let legacy = json!({"type": "model_response_received", "content": "Legacy visible response"});
    let event: RunEvent = serde_json::from_value(legacy.clone()).unwrap();
    assert!(matches!(
        &event,
        RunEvent::ModelResponseReceived {
            provider_state: None,
            provider_continuation: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(event).unwrap(), legacy);

    let message = ModelMessage::assistant("Legacy visible response".into(), None);
    let encoded = serde_json::to_value(&message).unwrap();
    assert_eq!(encoded["role"], "assistant");
    assert_eq!(
        serde_json::from_value::<ModelMessage>(encoded).unwrap(),
        message
    );
    let mut request = request();
    request.messages = vec![message];
    request
        .validate_provider_state_with(
            Some(&identity()),
            false,
            agent_adapters::validate_replay_state,
        )
        .unwrap();
    assert!(matches!(
        request.validate_provider_state_with(
            Some(&identity()),
            true,
            agent_adapters::validate_replay_state
        ),
        Err(ProviderReplayError::MissingState)
    ));
}

#[test]
fn replay_token_accounting_uses_declared_cost_and_never_ciphertext_length() {
    let mut input = request();
    let mut portable = input.clone();
    let ModelMessage {
        role: ModelRole::Assistant,
        provider_state,
        ..
    } = &mut portable.messages[1]
    else {
        unreachable!()
    };
    *provider_state = None;
    for counter in [
        ApproximateTokenCounter::default(),
        ApproximateTokenCounter::conservative(),
        ApproximateTokenCounter::new(1).unwrap(),
    ] {
        let visible_cost = counter.count_tokens(&portable);
        assert_eq!(counter.count_tokens(&input), visible_cost + 240);
        let ModelMessage {
            role: ModelRole::Assistant,
            provider_state: Some(state),
            ..
        } = &mut input.messages[1]
        else {
            unreachable!()
        };
        state.payload[0]["encrypted_content"] = "x".repeat(250_000).into();
        assert_eq!(counter.count_tokens(&input), visible_cost + 240);
        assert_eq!(
            counter.count_prefix_tokens(&input, 1),
            counter.count_prefix_tokens(&portable, 1)
        );
        let ModelMessage {
            role: ModelRole::Assistant,
            provider_state: Some(state),
            ..
        } = &mut input.messages[1]
        else {
            unreachable!()
        };
        state.replay_tokens = usize::MAX;
        assert_eq!(counter.count_tokens(&input), usize::MAX);
        let ModelMessage {
            role: ModelRole::Assistant,
            provider_state: Some(state),
            ..
        } = &mut input.messages[1]
        else {
            unreachable!()
        };
        state.replay_tokens = 240;
    }
}
