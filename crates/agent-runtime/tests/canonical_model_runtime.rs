mod support;

use std::{
    collections::VecDeque,
    convert::Infallible,
    fs,
    future::{Future, pending},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ContentBlock, ContextCapabilities, ContextCompactor,
    ContextSummary, EventStore, EventStoreHistoryAccess, HistoryAccess, HistoryContext,
    InMemoryEventStore, JsonlEventStore, Model, ModelBlockKind, ModelCapabilities,
    ModelContextCompactor, ModelFinishReason, ModelMessage, ModelOutput, ModelRequest,
    ModelResponse, ModelRole, ModelStreamEvent, ModelStreamSink, ProviderContextIdentity,
    ProviderContextWindow, ProviderReplayScope, ProviderResponseState, READ_RUN_HISTORY_TOOL,
    SEARCH_RUN_HISTORY_TOOL, ToolCall, ToolDefinition, ToolExecutor,
};
use serde_json::{Value, json};

fn finish(reason: ModelFinishReason) -> ModelStreamEvent {
    ModelStreamEvent::Finish {
        finish_reason: reason,
        provider_state: None,
        provider_context: None,
        context_compacted: false,
    }
}

fn call(arguments: &str) -> ContentBlock {
    ContentBlock::ToolCall {
        id: "call-1".into(),
        name: "read".into(),
        arguments: arguments.into(),
    }
}

fn assistant_content() -> Vec<ContentBlock> {
    vec![
        ContentBlock::Reasoning {
            text: "Inspect the requested file.".into(),
        },
        ContentBlock::Text {
            text: "Reading the file.".into(),
        },
        call("{}"),
        ContentBlock::Text {
            text: "Then I will summarize.".into(),
        },
    ]
}

fn interleaved_events() -> Vec<ModelStreamEvent> {
    let content = assistant_content();
    vec![
        ModelStreamEvent::BlockStart {
            index: 0,
            kind: ModelBlockKind::Reasoning,
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            text: "Inspect ".into(),
        },
        ModelStreamEvent::BlockStart {
            index: 1,
            kind: ModelBlockKind::Text,
        },
        ModelStreamEvent::TextDelta {
            index: 1,
            text: "Reading the file.".into(),
        },
        ModelStreamEvent::BlockStart {
            index: 2,
            kind: ModelBlockKind::ToolCall,
        },
        ModelStreamEvent::ToolCallDelta {
            index: 2,
            id: Some("call-1".into()),
            name: Some("read".into()),
            arguments_delta: "{".into(),
        },
        ModelStreamEvent::BlockEnd {
            index: 1,
            block: content[1].clone(),
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            text: "the requested file.".into(),
        },
        ModelStreamEvent::BlockEnd {
            index: 0,
            block: content[0].clone(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 2,
            id: None,
            name: None,
            arguments_delta: "}".into(),
        },
        ModelStreamEvent::BlockStart {
            index: 3,
            kind: ModelBlockKind::Text,
        },
        ModelStreamEvent::BlockEnd {
            index: 3,
            block: content[3].clone(),
        },
        ModelStreamEvent::BlockEnd {
            index: 2,
            block: content[2].clone(),
        },
        finish(ModelFinishReason::ToolCalls),
    ]
}

#[derive(Default)]
struct RecordingSink(Vec<ModelStreamEvent>);

impl ModelStreamSink for RecordingSink {
    fn emit(&mut self, event: ModelStreamEvent) {
        self.0.push(event);
    }
}

fn complete_events(response: ModelResponse) -> Vec<ModelStreamEvent> {
    let mut sink = RecordingSink::default();
    ModelOutput::from(response).emit_stream(&mut sink);
    sink.0
}

struct ScriptedStreamingModel {
    attempts: Mutex<VecDeque<Vec<ModelStreamEvent>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedStreamingModel {
    fn new(attempts: Vec<Vec<ModelStreamEvent>>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Model for ScriptedStreamingModel {
    type Error = Infallible;

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("the runtime must use the streaming adapter entry point")
    }

    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        for event in events {
            sink.emit(event);
        }
        Ok(())
    }
}

struct CountingTools {
    calls: AtomicUsize,
    definitions: Vec<ToolDefinition>,
}

impl CountingTools {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            definitions: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: "{}".into(),
            }],
        }
    }
}

impl ToolExecutor for CountingTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("file contents".into())
    }
}

#[tokio::test]
async fn ordered_stream_blocks_persist_and_replay_after_fresh_jsonl_restore() {
    let directory = TestDirectory::new();
    let agent = Agent::new("streaming", "streaming", "", "Follow the user.");
    let first_events = interleaved_events();
    let waiting_events = complete_events(ModelResponse::input_required("Continue?".into()));
    {
        let runtime = AgentRuntime::with_event_store(
            ScriptedStreamingModel::new(vec![first_events.clone(), waiting_events.clone()]),
            CountingTools::new(),
            support::test_context_manager(),
            JsonlEventStore::new(&directory.0),
            Some(4),
        );
        let mut run = runtime
            .start_run(&agent, "ordered-stream", "Inspect the file")
            .await
            .unwrap();
        let mut live = RecordingSink::default();
        assert_eq!(
            runtime
                .run_until_blocked_with_stream(&agent, &mut run, &mut live)
                .await
                .unwrap(),
            RunStatus::WaitingForInput
        );
        assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
        assert_eq!(live.0, [first_events, waiting_events].concat());
        let persisted = run
            .events()
            .iter()
            .find_map(|event| match &event.event {
                RunEvent::ModelResponseReceived {
                    response: Some(response),
                    ..
                } => Some(response),
                _ => None,
            })
            .unwrap();
        assert_eq!(persisted.content, assistant_content());
        let requests = runtime.model().requests.lock().unwrap();
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .find(|message| message.role == ModelRole::Assistant)
                .unwrap()
                .content,
            assistant_content()
        );
    }

    let runtime = AgentRuntime::with_event_store(
        ScriptedStreamingModel::new(vec![complete_events(ModelResponse::final_answer(
            "Done".into(),
        ))]),
        CountingTools::new(),
        support::test_context_manager(),
        JsonlEventStore::new(&directory.0),
        Some(4),
    );
    let mut run = runtime
        .restore_run("ordered-stream")
        .await
        .unwrap()
        .unwrap();
    runtime.submit_input(&mut run, "Continue").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
    let requests = runtime.model().requests.lock().unwrap();
    let messages = &requests[0].messages;
    assert_eq!(
        messages[1],
        ModelMessage::assistant_blocks(assistant_content(), None)
    );
    assert_eq!(
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::ToolCall { .. }))
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
            .count(),
        1
    );
}

#[derive(Clone, Copy)]
enum BadAttempt {
    MissingFinish,
    OpenBlock,
    ChangedContent,
    Length,
    TransportFailure,
}

struct BrokenStreamingModel(BadAttempt);

impl Model for BrokenStreamingModel {
    type Error = io::Error;

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("the runtime must use streaming")
    }

    async fn stream(
        &self,
        _: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        sink.emit(ModelStreamEvent::BlockStart {
            index: 0,
            kind: ModelBlockKind::ToolCall,
        });
        sink.emit(ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call-1".into()),
            name: Some("read".into()),
            arguments_delta: "{}".into(),
        });
        if !matches!(self.0, BadAttempt::OpenBlock) {
            let arguments = if matches!(self.0, BadAttempt::ChangedContent) {
                r#"{"changed":true}"#
            } else {
                "{}"
            };
            sink.emit(ModelStreamEvent::BlockEnd {
                index: 0,
                block: call(arguments),
            });
        }
        match self.0 {
            BadAttempt::TransportFailure => return Err(io::Error::other("transport interrupted")),
            BadAttempt::MissingFinish => {}
            BadAttempt::Length => sink.emit(finish(ModelFinishReason::Length)),
            _ => sink.emit(finish(ModelFinishReason::ToolCalls)),
        }
        Ok(())
    }
}

#[tokio::test]
async fn malformed_truncated_and_failed_streams_cannot_execute_tool_effects() {
    for mode in [
        BadAttempt::MissingFinish,
        BadAttempt::OpenBlock,
        BadAttempt::ChangedContent,
        BadAttempt::Length,
        BadAttempt::TransportFailure,
    ] {
        let agent = Agent::new("broken", "broken", "", "");
        let runtime = AgentRuntime::new(
            BrokenStreamingModel(mode),
            CountingTools::new(),
            support::test_context_manager(),
            Some(2),
        );
        let mut run = runtime
            .start_run(&agent, "broken-stream", "Read")
            .await
            .unwrap();
        assert!(runtime.run_until_blocked(&agent, &mut run).await.is_err());
        assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
        assert!(
            !run.events()
                .iter()
                .any(|event| matches!(event.event, RunEvent::ToolCallRequested { .. }))
        );
        if !matches!(mode, BadAttempt::Length) {
            assert!(
                !run.events()
                    .iter()
                    .any(|event| matches!(event.event, RunEvent::ModelResponseReceived { .. }))
            );
        }
    }
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct PendingStreamingModel(Arc<AtomicBool>);

impl Model for PendingStreamingModel {
    type Error = Infallible;

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("the runtime must use streaming")
    }

    async fn stream(
        &self,
        _: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        let _cleanup = DropFlag(Arc::clone(&self.0));
        sink.emit(ModelStreamEvent::BlockStart {
            index: 0,
            kind: ModelBlockKind::ToolCall,
        });
        sink.emit(ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call-1".into()),
            name: Some("read".into()),
            arguments_delta: "{".into(),
        });
        pending().await
    }
}

#[tokio::test]
async fn cancelling_a_live_stream_drops_the_adapter_and_never_commits_partial_calls() {
    let dropped = Arc::new(AtomicBool::new(false));
    let agent = Agent::new("cancel", "cancel", "", "");
    let runtime = AgentRuntime::new(
        PendingStreamingModel(Arc::clone(&dropped)),
        CountingTools::new(),
        support::test_context_manager(),
        Some(2),
    );
    let mut run = runtime
        .start_run(&agent, "cancel-stream", "Read")
        .await
        .unwrap();
    let mut cancel_handle = run.clone();
    let mut live = RecordingSink::default();
    let mut execution =
        Box::pin(runtime.run_until_blocked_with_stream(&agent, &mut run, &mut live));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(execution.as_mut().poll(&mut cx).is_pending());
    let mut cancellation = Box::pin(runtime.cancel_run(&mut cancel_handle));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        execution.as_mut().poll(&mut cx),
        Poll::Ready(Ok(RunStatus::Cancelled))
    ));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(execution);
    drop(cancellation);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
    assert_eq!(live.0.len(), 2);
    assert!(!run.events().iter().any(|event| matches!(
        event.event,
        RunEvent::ModelResponseReceived { .. } | RunEvent::ToolCallRequested { .. }
    )));
}

const PRIVATE_REASONING: &str = "canonical-reasoning-must-not-enter-history-tools";
const PRIVATE_STATE: &str = "canonical-opaque-state-must-not-enter-summary";

struct SummaryInputSpy;

impl Model for SummaryInputSpy {
    type Error = Infallible;

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains(PRIVATE_REASONING));
        assert!(!encoded.contains(PRIVATE_STATE));
        assert!(encoded.contains("Visible answer"));
        Ok(ModelResponse::final_answer(
            serde_json::to_string(&ContextSummary {
                objective: "Continue the visible task".into(),
                ..ContextSummary::default()
            })
            .unwrap(),
        ))
    }
}

#[tokio::test]
async fn canonical_reasoning_is_durable_but_excluded_from_history_tools_and_summary_input() {
    let mut run = AgentRun::new("canonical-private-content", "agent", "Inspect");
    run.begin_step().unwrap();
    let response = ModelResponse {
        content: vec![
            ContentBlock::Reasoning {
                text: PRIVATE_REASONING.into(),
            },
            ContentBlock::Text {
                text: "Visible answer".into(),
            },
        ],
        finish_reason: ModelFinishReason::Stop,
    };
    run.record(RunEvent::ModelResponseReceived {
        content: response.text(),
        response: Some(response),
        provider_state: Some(
            ProviderResponseState::new(
                ProviderContextIdentity {
                    provider: "fixture".into(),
                    model: "model".into(),
                    format: "adapter-v1".into(),
                },
                "custom-protocol",
                ProviderReplayScope::Response,
                json!({"opaque": PRIVATE_STATE}),
                10,
            )
            .unwrap(),
        ),
        provider_continuation: None,
        token_usage: None,
    })
    .unwrap();
    let original = run.events().to_vec();
    let store = InMemoryEventStore::new();
    for event in &original {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
    let access = EventStoreHistoryAccess::new();
    let context = HistoryContext {
        run_id: run.id().into(),
        through_sequence: original.last().unwrap().sequence,
        checkpoint_id: "checkpoint".into(),
    };
    for (tool_name, arguments) in [
        (SEARCH_RUN_HISTORY_TOOL, json!({"query": PRIVATE_REASONING})),
        (SEARCH_RUN_HISTORY_TOOL, json!({"query": PRIVATE_STATE})),
        (
            READ_RUN_HISTORY_TOOL,
            json!({"from_sequence": 1, "to_sequence": context.through_sequence}),
        ),
    ] {
        let output = access
            .execute(
                &store,
                context.clone(),
                ToolCall {
                    id: "history-call".into(),
                    name: tool_name.into(),
                    arguments: arguments.to_string(),
                },
            )
            .await
            .unwrap();
        assert!(!output.contains(PRIVATE_REASONING));
        assert!(!output.contains(PRIVATE_STATE));
        if tool_name == SEARCH_RUN_HISTORY_TOOL {
            let result: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(result["total_matches"], 0);
        } else {
            assert!(output.contains("Visible answer"));
        }
    }
    ModelContextCompactor::new(SummaryInputSpy)
        .compact(None, original.clone())
        .await
        .unwrap();
    let persisted = store.load(run.id()).await.unwrap().unwrap().events;
    assert_eq!(persisted, original);
    let encoded = serde_json::to_string(&persisted).unwrap();
    assert!(encoded.contains(PRIVATE_REASONING));
    assert!(encoded.contains(PRIVATE_STATE));
}

struct UndeclaredNativeOutputModel {
    identity: ProviderContextIdentity,
    accepts_native_context: bool,
    context_compacted: bool,
}

impl Model for UndeclaredNativeOutputModel {
    type Error = Infallible;

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            context: ContextCapabilities {
                accepts_native_context: self.accepts_native_context,
                supports_native_compaction: false,
                ..ContextCapabilities::default()
            },
            ..ModelCapabilities::default()
        }
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("metadata response must be used")
    }

    async fn respond_with_metadata(&self, _: ModelRequest) -> Result<ModelOutput, Self::Error> {
        Ok(ModelOutput {
            response: ModelResponse {
                content: vec![call("{}")],
                finish_reason: ModelFinishReason::ToolCalls,
            },
            usage: None,
            provider_state: None,
            provider_context: Some(ProviderContextWindow {
                identity: self.identity.clone(),
                items: vec![json!({"native": "complete-window"})],
                input_tokens: 10,
                prefix_fingerprint: None,
            }),
            context_compacted: self.context_compacted,
        })
    }
}

#[tokio::test]
async fn undeclared_native_windows_and_compaction_fail_before_commit_or_tool_effects() {
    // Replay identity alone does not authorize native windows; accepting
    // windows also does not automatically authorize provider-side compaction.
    for (accepts_native_context, context_compacted) in [(false, false), (true, true)] {
        let agent = Agent::new("native-capabilities", "native-capabilities", "", "");
        let model = UndeclaredNativeOutputModel {
            identity: ProviderContextIdentity {
                provider: "fixture".into(),
                model: "model".into(),
                format: "native-v1".into(),
            },
            accepts_native_context,
            context_compacted,
        };
        let runtime = AgentRuntime::new(
            model,
            CountingTools::new(),
            support::test_context_manager(),
            Some(2),
        );
        let mut run = runtime
            .start_run(&agent, "undeclared-native", "Read")
            .await
            .unwrap();
        assert!(matches!(
            runtime.run_until_blocked(&agent, &mut run).await,
            Err(AgentRuntimeError::ProviderContextMismatch)
        ));
        assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
        assert!(!run.events().iter().any(|event| matches!(
            event.event,
            RunEvent::ModelResponseReceived { .. } | RunEvent::ToolCallRequested { .. }
        )));
        let stored = runtime.event_store().load(run.id()).await.unwrap().unwrap();
        assert_eq!(stored.events, run.events());
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "openharness-canonical-stream-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
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
