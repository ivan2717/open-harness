use agent_adapters::WireProtocol;
use agent_runtime::{ContentBlock, MessageContext, ModelRole};
use std::{
    convert::Infallible,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunStatus};
use agent_runtime::*;
use serde_json::json;

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "native-fixture".into(),
        model: "model-v1".into(),
        format: "messages-v1".into(),
    }
}

fn business_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "read".into(),
        description: "Read a record".into(),
        input_schema: "{}".into(),
    }]
}

fn deferred_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "history".into(),
        description: "Read omitted history".into(),
        input_schema: "{}".into(),
    }]
}

fn window(tokens: usize, marker: &str) -> ProviderContextWindow {
    ProviderContextWindow {
        identity: identity(),
        items: vec![json!({"type":"compaction", "encrypted_content":marker})],
        input_tokens: tokens,
        prefix_fingerprint: None,
    }
}

struct InitialContext;
impl ContextManager for InitialContext {
    type Error = Infallible;
    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        let RunEvent::RunStarted { instruction, .. } = &input.events[0].event else {
            unreachable!()
        };
        Ok(PreparedContext {
            request: ModelRequest {
                instructions: input.instructions.into(),
                tools: input.tools.to_vec(),
                messages: vec![ModelMessage::user(instruction.clone())],
                provider_context: None,
                max_output_tokens: None,
            },
            omitted_history: None,
        })
    }
}

struct Tools(Vec<ToolDefinition>);
impl ToolExecutor for Tools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.0
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        panic!("fixture must not execute a tool")
    }
}

struct PrefixModel {
    identity: ProviderContextIdentity,
    tokens: usize,
}
impl Model for PrefixModel {
    type Error = Infallible;
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), agent_runtime::ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        unreachable!()
    }
    async fn respond_with_metadata(&self, _: ModelRequest) -> Result<ModelOutput, Self::Error> {
        Ok(ModelOutput {
            response: ModelResponse::input_required("Which record?".into()),
            provider_state: Some(WireProtocol::AnthropicMessages.capture_state(identity(),
                ProviderReplayScope::Prefix,
                json!({"role":"assistant", "content":[{"type":"thinking", "thinking":"", "signature":"signed-state"}, {"type":"text", "text":"Which record?"}]}), 20).unwrap()),
            provider_context: Some(window(self.tokens, "original-window")),
            usage: None, context_compacted: false,
        })
    }
}

async fn prefix_history(tokens: usize) -> AgentRun {
    let directory = TestDirectory::new();
    let runtime = AgentRuntime::with_event_store(
        PrefixModel {
            identity: identity(),
            tokens,
        },
        Tools(business_tools()),
        InitialContext,
        JsonlEventStore::new(&directory.0),
        None,
    );
    let agent = Agent::new("agent", "Agent", "Fixture", "Stable instructions");
    let mut run = runtime
        .start_run(&agent, "prefix-native", "Inspect the record")
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    drop(runtime);
    let stored = JsonlEventStore::new(&directory.0)
        .load(run.id())
        .await
        .unwrap()
        .unwrap();
    AgentRun::restore(stored.run_id, stored.agent_id, stored.events).unwrap()
}

fn input(run: &AgentRun) -> ContextInput {
    ContextInput::new(
        run.id(),
        "Stable instructions",
        run.events().to_vec(),
        business_tools(),
    )
    .with_deferred_tools(deferred_tools())
}

#[derive(Clone)]
struct SpyCompactor {
    calls: Arc<Mutex<Vec<ModelRequest>>>,
    result: ProviderContextWindow,
    identity: ProviderContextIdentity,
}
impl NativeContextCompactor for SpyCompactor {
    type Error = Infallible;
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), agent_runtime::ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }
    fn identity(&self) -> &ProviderContextIdentity {
        &self.identity
    }
    fn input_token_limit(&self) -> usize {
        20_000
    }
    async fn compact(
        &self,
        request: ModelRequest,
        _: usize,
    ) -> Result<ProviderContextWindow, Self::Error> {
        self.calls.lock().unwrap().push(request);
        Ok(self.result.clone())
    }
}

fn manager<K>(
    store: K,
    limit: usize,
    threshold: usize,
) -> NativeContextManager<SpyCompactor, K, ApproximateTokenCounter> {
    NativeContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(limit)
            .compaction_threshold(threshold)
            .compaction_target(threshold / 2)
            .recent_tail_tokens(100)
            .max_tool_output_chars(4000)
            .build()
            .unwrap(),
        SpyCompactor {
            calls: Arc::default(),
            result: window(20, "compacted-window"),
            identity: identity(),
        },
        store,
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

fn latest_window(run: &AgentRun) -> ProviderContextWindow {
    run.events()
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            RunEvent::ModelResponseReceived {
                provider_continuation: Some(continuation),
                ..
            } => Some(continuation.window.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn prefix_window_keeps_original_tools_instead_of_adding_history_tools() {
    let run = prefix_history(40).await;
    let expected = latest_window(&run);
    assert!(expected.prefix_fingerprint.is_some());
    let manager = manager(InMemoryNativeContextCheckpointStore::new(), 10_000, 9000);
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();
    assert_eq!(prepared.request.provider_context, Some(expected));
    assert_eq!(prepared.request.tools, business_tools());
    prepared
        .request
        .validate_provider_state(Some(&identity()), true)
        .unwrap();
    assert!(manager.compactor().calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn changed_instructions_or_business_tools_fail_before_native_compaction() {
    let run = prefix_history(4000).await;
    for change_tools in [false, true] {
        let manager = manager(InMemoryNativeContextCheckpointStore::new(), 10_000, 1000);
        let mut input = input(&run);
        if change_tools {
            input.tools[0].description = "Different tool description".into();
        } else {
            input.instructions = "Different instructions".into();
        }
        assert!(matches!(
            manager.prepare_owned(input).await,
            Err(NativeContextError::Replay(
                ProviderReplayError::PrefixChanged
            ))
        ));
        assert!(manager.compactor().calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn native_compaction_preserves_fixed_binding_on_its_replacement_window() {
    let run = prefix_history(4000).await;
    let expected_binding = latest_window(&run).prefix_fingerprint;
    let manager = manager(InMemoryNativeContextCheckpointStore::new(), 10_000, 1000);
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();
    let returned = prepared.request.provider_context.as_ref().unwrap();
    assert_eq!(returned.prefix_fingerprint, expected_binding);
    assert_eq!(returned.items, window(20, "compacted-window").items);
    assert_eq!(prepared.request.tools, business_tools());
    assert_eq!(manager.compactor().calls.lock().unwrap().len(), 1);
}

struct BrokenCache;
impl NativeContextCheckpointStore for BrokenCache {
    type Error = io::Error;
    async fn latest(&self, _: String) -> Result<Option<NativeContextCheckpoint>, Self::Error> {
        Err(io::Error::other("cache unavailable"))
    }
    async fn save(&self, _: NativeContextCheckpoint) -> Result<(), Self::Error> {
        Err(io::Error::other("cache unavailable"))
    }
    async fn clear(&self, _: String) -> Result<(), Self::Error> {
        Err(io::Error::other("cache unavailable"))
    }
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "open-harness-native-state-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn journal_tool_history(compacted: bool) -> AgentRun {
    let mut run = AgentRun::new("journal", "agent", "Inspect both records");
    run.begin_step().unwrap();
    let mut native = window(40, "server-compaction-state");
    native.items.extend([
        json!({"type":"function_call", "call_id":"a", "name":"read", "arguments":"{}"}),
        json!({"type":"function_call", "call_id":"b", "name":"read", "arguments":"{}"}),
    ]);
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        content: "Inspecting".into(),
        token_usage: None,
        provider_state: None,
        provider_continuation: Some(Box::new(ProviderContinuation {
            window: native,
            compacted,
        })),
    })
    .unwrap();
    for id in ["a", "b"] {
        run.record(RunEvent::ToolCallRequested {
            call_id: id.into(),
            tool_name: "read".into(),
            arguments: "{}".into(),
        })
        .unwrap();
    }
    for id in ["a", "b"] {
        run.record(RunEvent::ToolCallSucceeded {
            call_id: id.into(),
            output: format!("result {id}"),
            images: Vec::new(),
        })
        .unwrap();
    }
    run
}

#[tokio::test]
async fn journal_recovers_exact_server_window_without_cache_or_duplicate_calls() {
    for compacted in [false, true] {
        let original = journal_tool_history(compacted);
        let directory = TestDirectory::new();
        {
            let store = JsonlEventStore::new(&directory.0);
            for event in original.events() {
                store
                    .append(original.id(), original.agent_id(), event)
                    .await
                    .unwrap();
            }
        }
        // Reopen canonical events with a fresh store and no derived cache.
        let stored = JsonlEventStore::new(&directory.0)
            .load(original.id())
            .await
            .unwrap()
            .unwrap();
        let restored = AgentRun::restore(stored.run_id, stored.agent_id, stored.events).unwrap();
        let manager = manager(BrokenCache, 10_000, 9000);
        let prepared = manager.prepare_owned(input(&restored)).await.unwrap();
        assert_eq!(
            prepared.request.provider_context,
            Some(latest_window(&original))
        );
        assert_eq!(prepared.omitted_history.unwrap().through_sequence, 4);
        assert_eq!(
            prepared
                .request
                .messages
                .iter()
                .filter(|message| message.content.iter().any(|block| matches!(
                    block,
                    ContentBlock::ToolResult {
                        is_error: false,
                        ..
                    }
                )))
                .count(),
            2
        );
        assert!(!prepared.request.messages.iter().any(|message| matches!(
            message,
            ModelMessage {
                role: ModelRole::Assistant,
                ..
            }
        )));
        assert_eq!(
            prepared
                .request
                .messages
                .iter()
                .filter(|message| {
                    let candidate = message;
                    matches!(&candidate.context, Some(MessageContext::Protected))
                })
                .count(),
            usize::from(compacted)
        );
        assert!(manager.compactor().calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn failed_native_cache_write_cannot_discard_a_durable_continuation_to_fit() {
    let run = journal_tool_history(false);
    let mut events = run.events().to_vec();
    let RunEvent::ModelResponseReceived {
        provider_continuation: Some(continuation),
        ..
    } = &mut events[2].event
    else {
        unreachable!()
    };
    continuation.window.input_tokens = 4000;
    let restored = AgentRun::restore(run.id(), run.agent_id(), events).unwrap();
    let manager = manager(BrokenCache, 1000, 800);
    assert!(matches!(
        manager.prepare_owned(input(&restored)).await,
        Err(NativeContextError::CheckpointStore(_))
    ));
    assert_eq!(manager.compactor().calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn canonical_continuation_overrides_an_older_cached_window() {
    let mut run = journal_tool_history(false);
    let manager = manager(InMemoryNativeContextCheckpointStore::new(), 10_000, 9000);
    manager
        .observe_response(
            input(&run).as_ref(),
            &ModelOutput {
                response: ModelResponse::final_answer("Old".into()),
                provider_state: None,
                provider_context: Some(window(30, "old-cache")),
                usage: None,
                context_compacted: false,
            },
        )
        .await
        .unwrap();
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        content: "Latest".into(),
        token_usage: None,
        provider_state: None,
        provider_continuation: Some(Box::new(ProviderContinuation {
            window: window(35, "latest-journal"),
            compacted: false,
        })),
    })
    .unwrap();
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();
    assert_eq!(
        prepared.request.provider_context,
        Some(window(35, "latest-journal"))
    );
    assert!(prepared.request.messages.is_empty());
}

#[tokio::test]
async fn invalid_local_state_cannot_be_sent_to_compactor_or_hidden_by_fallback() {
    let mut run = AgentRun::new("invalid", "agent", "Inspect");
    run.begin_step().unwrap();
    let state = WireProtocol::ChatCompletions
        .capture_state(
            identity(),
            ProviderReplayScope::Response,
            json!({"role":"assistant", "content":"different"}),
            2000,
        )
        .unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        content: "Visible".into(),
        provider_state: Some(state),
        provider_continuation: None,
        token_usage: None,
    })
    .unwrap();
    let manager = manager(InMemoryNativeContextCheckpointStore::new(), 10_000, 1000);
    assert!(matches!(
        manager.prepare_owned(input(&run)).await,
        Err(NativeContextError::Replay(
            ProviderReplayError::InvalidResponse(ProviderStateError::ContentMismatch)
        ))
    ));
    assert!(manager.compactor().calls.lock().unwrap().is_empty());
}
