use agent_adapters::WireProtocol;
use agent_runtime::ContentBlock;
mod support;

use std::{
    collections::VecDeque,
    convert::Infallible,
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{Agent, RunEvent, RunEventEnvelope, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, AppendOutcome, EventStore, EventStoreError, JsonlEventStore,
    Model, ModelMessage, ModelOutput, ModelRequest, ModelResponse, ProviderContextIdentity,
    ProviderReplayScope, ProviderResponseState, StoredRun, ToolCall, ToolDefinition, ToolExecutor,
};
use serde_json::json;
use support::test_context_manager;

fn protocols() -> [WireProtocol; 2] {
    [WireProtocol::Responses, WireProtocol::GeminiGenerateContent]
}

fn identity(protocol: WireProtocol) -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: format!("fixture-{protocol:?}"),
        model: "reasoning-model".into(),
        format: "lossless-v1".into(),
    }
}

fn state(
    protocol: WireProtocol,
    scope: ProviderReplayScope,
    content: &str,
    tool_call: bool,
) -> ProviderResponseState {
    let payload = match protocol {
        WireProtocol::Responses => {
            let mut items = vec![
                json!({
                    "type": "reasoning", "id": "reasoning-1",
                    "encrypted_content": "opaque\nstate\u{0}+/=",
                    "summary": [{"type": "summary_text", "text": "Private summary"}],
                    "future_field": {"retained": true}
                }),
                json!({
                    "type": "message", "id": "message-1", "role": "assistant",
                    "content": [{"type": "output_text", "text": content, "annotations": []}]
                }),
            ];
            if tool_call {
                items.push(json!({
                    "type": "function_call", "id": "function-1", "call_id": "call-1",
                    "name": "write_record", "arguments": "{\"value\":1}",
                    "status": "completed"
                }));
            }
            json!(items)
        }
        WireProtocol::GeminiGenerateContent => {
            let mut parts = vec![
                json!({
                    "text": "Private reasoning", "thought": true,
                    "thoughtSignature": "opaque-thought-signature+/="
                }),
                json!({"text": content}),
            ];
            if tool_call {
                parts.push(json!({
                    "functionCall": {
                        "id": "call-1", "name": "write_record", "args": {"value": 1}
                    },
                    "thoughtSignature": "opaque-call-signature+/="
                }));
            }
            json!({"role": "model", "parts": parts, "future_field": {"retained": true}})
        }
        _ => unreachable!(),
    };
    protocol
        .capture_state(identity(protocol), scope, payload, 37)
        .unwrap()
}

fn final_output(protocol: WireProtocol) -> ModelOutput {
    ModelOutput {
        response: ModelResponse::final_answer("Done".into()),
        provider_state: Some(state(protocol, ProviderReplayScope::Prefix, "Done", false)),
        usage: None,
        provider_context: None,
        context_compacted: false,
    }
}

struct ReplayModel {
    identity: ProviderContextIdentity,
    outputs: Mutex<VecDeque<ModelOutput>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ReplayModel {
    fn new(protocol: WireProtocol, outputs: Vec<ModelOutput>) -> Self {
        Self {
            identity: identity(protocol),
            outputs: Mutex::new(outputs.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Model for ReplayModel {
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
        unreachable!("provider metadata must be retained")
    }

    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .outputs
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted"))
    }
}

struct RecordingTools {
    definitions: Vec<ToolDefinition>,
    executions: AtomicU64,
}

impl RecordingTools {
    fn new() -> Self {
        Self {
            definitions: vec![ToolDefinition {
                name: "write_record".into(),
                description: "Write one record".into(),
                input_schema: r#"{"type":"object","properties":{"value":{"type":"integer"}}}"#
                    .into(),
            }],
            executions: AtomicU64::new(0),
        }
    }
}

impl ToolExecutor for RecordingTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok("stored value 1".into())
    }
}

fn agent() -> Agent {
    Agent::new(
        "stateful-agent",
        "Stateful agent",
        "Persistence fixture",
        "Follow the request.",
    )
}

fn persisted_state(events: &[RunEventEnvelope]) -> &ProviderResponseState {
    events
        .iter()
        .find_map(|envelope| match &envelope.event {
            RunEvent::ModelResponseReceived {
                provider_state: Some(state),
                ..
            } => Some(state),
            _ => None,
        })
        .expect("persisted response state")
}

#[tokio::test]
async fn waiting_input_state_survives_fresh_jsonl_store_and_model_instances() {
    for protocol in protocols() {
        let directory = TestDirectory::new();
        let expected = state(
            protocol,
            ProviderReplayScope::Prefix,
            "Which record?",
            false,
        );
        let bound_state = {
            let runtime = AgentRuntime::with_event_store(
                ReplayModel::new(
                    protocol,
                    vec![ModelOutput {
                        response: ModelResponse {
                            content: protocol
                                .decode_content(&expected.payload, "persisted-response")
                                .unwrap(),
                            finish_reason: agent_runtime::ModelFinishReason::InputRequired,
                        },
                        provider_state: Some(expected.clone()),
                        usage: None,
                        provider_context: None,
                        context_compacted: false,
                    }],
                ),
                RecordingTools::new(),
                test_context_manager(),
                JsonlEventStore::new(&directory.0),
                None,
            );
            let original_agent = agent();
            let mut run = runtime
                .start_run(&original_agent, "waiting", "Inspect a record")
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .run_until_blocked(&original_agent, &mut run)
                    .await
                    .unwrap(),
                RunStatus::WaitingForInput
            );
            let state = persisted_state(run.events());
            assert_eq!(state.payload, expected.payload);
            assert!(state.prefix_fingerprint.is_some());
            state.clone()
        };

        // Nothing from the first runtime, model, context manager, or store is
        // reused. The provider payload comes solely from the reopened log.
        let runtime = AgentRuntime::with_event_store(
            ReplayModel::new(protocol, vec![final_output(protocol)]),
            RecordingTools::new(),
            test_context_manager(),
            JsonlEventStore::new(&directory.0),
            None,
        );
        let restored_agent = agent();
        let mut run = runtime.restore_run("waiting").await.unwrap().unwrap();
        assert_eq!(run.status(), RunStatus::WaitingForInput);
        assert_eq!(persisted_state(run.events()), &bound_state);
        runtime.submit_input(&mut run, "Record 1").await.unwrap();
        assert_eq!(
            runtime
                .run_until_blocked(&restored_agent, &mut run)
                .await
                .unwrap(),
            RunStatus::Completed
        );
        let requests = runtime.model().requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].messages,
            vec![
                ModelMessage::user("Inspect a record".into()),
                ModelMessage::assistant_blocks(
                    protocol
                        .decode_content(&bound_state.payload, "persisted-response")
                        .unwrap(),
                    Some(bound_state)
                ),
                ModelMessage::user("Record 1".into()),
            ]
        );
        assert_eq!(runtime.tools().executions.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn completed_tool_step_restores_state_without_reexecuting_the_tool() {
    for protocol in protocols() {
        let directory = TestDirectory::new();
        let expected = state(
            protocol,
            ProviderReplayScope::Response,
            "Writing the record",
            true,
        );
        {
            let store = JsonlEventStore::new(&directory.0);
            let mut run = agent().start_run("completed-tool", "Write record 1");
            run.begin_step().unwrap();
            run.record(RunEvent::ModelResponseReceived {
                response: None,
                provider_continuation: None,
                content: "Writing the record".into(),
                provider_state: Some(expected.clone()),
            })
            .unwrap();
            run.record(RunEvent::ToolCallRequested {
                call_id: "call-1".into(),
                tool_name: "write_record".into(),
                arguments: "{\"value\":1}".into(),
            })
            .unwrap();
            run.record(RunEvent::ToolCallSucceeded {
                call_id: "call-1".into(),
                output: "stored value 1".into(),
            })
            .unwrap();
            for event in run.events() {
                store.append(run.id(), run.agent_id(), event).await.unwrap();
            }
        }

        let runtime = AgentRuntime::with_event_store(
            ReplayModel::new(protocol, vec![final_output(protocol)]),
            RecordingTools::new(),
            test_context_manager(),
            JsonlEventStore::new(&directory.0),
            None,
        );
        let mut run = runtime
            .restore_run("completed-tool")
            .await
            .unwrap()
            .unwrap();
        assert!(run.can_begin_step());
        assert_eq!(persisted_state(run.events()), &expected);
        assert_eq!(
            runtime.run_until_blocked(&agent(), &mut run).await.unwrap(),
            RunStatus::Completed
        );
        let requests = runtime.model().requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].messages,
            vec![
                ModelMessage::user("Write record 1".into()),
                ModelMessage::assistant_blocks(
                    ModelResponse::tool_calls(
                        "Writing the record".into(),
                        vec![ToolCall {
                            id: "call-1".into(),
                            name: "write_record".into(),
                            arguments: "{\"value\":1}".into()
                        }]
                    )
                    .content,
                    Some(expected)
                ),
                ModelMessage::tool_result("call-1".into(), "stored value 1".into(), false),
            ]
        );
        assert_eq!(runtime.tools().executions.load(Ordering::SeqCst), 0);
        assert_eq!(
            run.events()
                .iter()
                .filter(|event| matches!(event.event, RunEvent::ToolCallSucceeded { .. }))
                .count(),
            1
        );
    }
}

struct FailResponseStore {
    inner: JsonlEventStore,
    append_before_failure: bool,
}

impl EventStore for FailResponseStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        if matches!(event.event, RunEvent::ModelResponseReceived { .. }) {
            if self.append_before_failure {
                self.inner.append(run_id, agent_id, event).await?;
            }
            return Err(EventStoreError::backend(std::io::Error::other(
                "response durability not confirmed",
            )));
        }
        self.inner.append(run_id, agent_id, event).await
    }

    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.inner.load(run_id).await
    }
}

#[tokio::test]
async fn response_and_provider_state_commit_together_before_any_tool_effect() {
    for append_before_failure in [false, true] {
        let directory = TestDirectory::new();
        let protocol = WireProtocol::Responses;
        let expected = state(
            protocol,
            ProviderReplayScope::Prefix,
            "Writing the record",
            true,
        );
        let runtime = AgentRuntime::with_event_store(
            ReplayModel::new(
                protocol,
                vec![ModelOutput {
                    response: ModelResponse {
                        content: protocol
                            .decode_content(&expected.payload, "persisted-response")
                            .unwrap(),
                        finish_reason: agent_runtime::ModelFinishReason::ToolCalls,
                    },
                    provider_state: Some(expected.clone()),
                    usage: None,
                    provider_context: None,
                    context_compacted: false,
                }],
            ),
            RecordingTools::new(),
            test_context_manager(),
            FailResponseStore {
                inner: JsonlEventStore::new(&directory.0),
                append_before_failure,
            },
            None,
        );
        let mut run = runtime
            .start_run(&agent(), "failed-commit", "Write record 1")
            .await
            .unwrap();
        let error = runtime
            .run_until_blocked(&agent(), &mut run)
            .await
            .unwrap_err();
        assert!(matches!(error, AgentRuntimeError::EventStore(_)));
        assert_eq!(runtime.tools().executions.load(Ordering::SeqCst), 0);
        assert_eq!(run.events().len(), 2);
        let stored = JsonlEventStore::new(&directory.0)
            .load("failed-commit")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.events.len(),
            if append_before_failure { 3 } else { 2 }
        );
        assert!(
            !stored
                .events
                .iter()
                .any(|event| matches!(event.event, RunEvent::ToolCallRequested { .. }))
        );
        if append_before_failure {
            let RunEvent::ModelResponseReceived {
                content,
                provider_state: Some(state),
                ..
            } = &stored.events[2].event
            else {
                panic!("visible response and provider state must share one durable event")
            };
            assert_eq!(content, "Writing the record");
            let RunEvent::ModelResponseReceived {
                response: Some(response),
                ..
            } = &stored.events[2].event
            else {
                panic!("canonical blocks must share the durable response event")
            };
            assert_eq!(
                response.content,
                protocol
                    .decode_content(&expected.payload, "persisted-response")
                    .unwrap()
            );
            assert!(
                response
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
            );
            assert_eq!(state.payload, expected.payload);
            assert!(state.prefix_fingerprint.is_some());
        }
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "provider-state-{}-{timestamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
