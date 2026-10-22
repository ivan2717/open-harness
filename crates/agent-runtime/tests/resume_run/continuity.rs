use super::*;
use agent_core::agent::RunEventEnvelope;
use agent_runtime::{
    AppendOutcome, EventStoreError, InMemoryEventStore, ModelOutput, ProviderResponseState,
    ProviderStateError, StoredRun,
};
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct PrefixModel {
    identity: ProviderContextIdentity,
    calls: AtomicUsize,
}
impl Model for PrefixModel {
    type Error = Infallible;
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        unreachable!()
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            request
                .validate_provider_state_with(
                    Some(&self.identity),
                    true,
                    agent_adapters::validate_replay_state,
                )
                .unwrap();
            return Ok(ModelResponse::final_answer("Continued safely".into()).into());
        }
        let protocol = WireProtocol::AnthropicMessages;
        let payload = json!({"role":"assistant", "content":[
            {"type":"thinking", "thinking":"Use the exact context", "signature":"signature-must-survive"},
            {"type":"tool_use", "id":"write-1", "name":"write", "input":{}},
        ]});
        Ok(ModelOutput {
            response: ModelResponse {
                content: protocol.decode_content(&payload, "prefix").unwrap(),
                finish_reason: ModelFinishReason::ToolCalls,
            },
            provider_state: Some(
                protocol
                    .capture_state(
                        self.identity.clone(),
                        ProviderReplayScope::Prefix,
                        payload,
                        30,
                    )
                    .unwrap(),
            ),
            usage: None,
            provider_context: None,
            context_compacted: false,
        })
    }
}

struct PendingTools {
    definitions: Vec<ToolDefinition>,
    calls: AtomicUsize,
}
impl ToolExecutor for PendingTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        pending().await
    }
}

#[tokio::test]
async fn prefix_bound_reasoning_survives_real_execution_interruption_and_resume() {
    let runtime = AgentRuntime::new(
        PrefixModel {
            identity: ProviderContextIdentity {
                provider: "fixture".into(),
                model: "prefix-model".into(),
                format: "messages-v1".into(),
            },
            calls: AtomicUsize::new(0),
        },
        PendingTools {
            definitions: vec![ToolDefinition {
                name: "write".into(),
                description: "Write one file".into(),
                input_schema: "{}".into(),
            }],
            calls: AtomicUsize::new(0),
        },
        support::test_context_manager(),
        Some(1),
    );
    let agent = agent();
    let mut run = runtime
        .start_run(&agent, "prefix-interrupted", "Work")
        .await
        .unwrap();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    assert!(
        execution
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
    drop(execution);
    let state = run
        .events()
        .iter()
        .find_map(|entry| match &entry.event {
            RunEvent::ModelResponseReceived { provider_state, .. } => provider_state.clone(),
            _ => None,
        })
        .unwrap();
    assert!(state.prefix_fingerprint.is_some());
    runtime
        .resume_run(&mut run, "Inspect the file before any retry")
        .await
        .unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        run.events().iter().find_map(|entry| match &entry.event {
            RunEvent::ModelResponseReceived { provider_state, .. } => provider_state.as_ref(),
            _ => None,
        }),
        Some(&state)
    );
}

struct FailingRecoveryStore {
    inner: InMemoryEventStore,
    fail_once: AtomicBool,
    commit_before_failure: bool,
}
impl EventStore for FailingRecoveryStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        if matches!(event.event, RunEvent::RunResumed { .. })
            && self.fail_once.swap(false, Ordering::SeqCst)
        {
            if self.commit_before_failure {
                self.inner.append(run_id, agent_id, event).await?;
            }
            return Err(EventStoreError::backend(std::io::Error::other(
                "injected recovery durability failure",
            )));
        }
        self.inner.append(run_id, agent_id, event).await
    }
    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.inner.load(run_id).await
    }
}

#[tokio::test]
async fn interrupted_recovery_commit_never_leaves_half_settled_calls() {
    for commit_before_failure in [false, true] {
        let store = FailingRecoveryStore {
            inner: InMemoryEventStore::new(),
            fail_once: AtomicBool::new(true),
            commit_before_failure,
        };
        let runtime = AgentRuntime::with_event_store(
            ModelSpy::default(),
            NoRepeatTools,
            support::test_context_manager(),
            store,
            None,
        );
        let mut run = runtime
            .start_run(
                &agent(),
                format!("durability-{commit_before_failure}"),
                "Work",
            )
            .await
            .unwrap();
        run.begin_step().unwrap();
        response(&mut run);
        declare(&mut run, 1);
        let original = run.events().to_vec();
        assert!(matches!(
            runtime.resume_run(&mut run, "Continue carefully").await,
            Err(AgentRuntimeError::EventStore(_))
        ));
        assert_eq!(run.events(), original);
        let mut restored = runtime.restore_run(run.id()).await.unwrap().unwrap();
        if commit_before_failure {
            assert!(restored.interrupted_tool_calls().is_empty());
            assert!(restored.can_begin_step());
        } else {
            assert_eq!(restored.interrupted_tool_calls().len(), 2);
            assert!(!restored.can_begin_step());
            runtime
                .resume_run(&mut restored, "Continue carefully")
                .await
                .unwrap();
        }
        runtime
            .run_until_blocked(&agent(), &mut restored)
            .await
            .unwrap();
        assert_eq!(restored.status(), RunStatus::Completed);
    }
}
