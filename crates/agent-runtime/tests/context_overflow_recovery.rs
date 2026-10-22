use std::{
    collections::VecDeque,
    convert::Infallible,
    future::{Future, pending},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ContentBlock, ContextInputRef, ContextManager, Model,
    ModelBlockKind, ModelContextMetadata, ModelMessage, ModelOutput, ModelRequest, ModelResponse,
    ModelStreamEvent, ModelStreamSink, ModelUsage, PreparedContext, ProviderContextIdentity,
    ToolCall, ToolDefinition, ToolExecutor, request_usage_baseline,
};

#[derive(Clone, Copy)]
enum Attempt {
    Overflow,
    PartialOverflow,
    UsageOverflow,
    OtherError,
    Tool,
    Final,
    Pending,
}

struct ScriptModel {
    attempts: Mutex<VecDeque<Attempt>>,
    requests: Mutex<Vec<ModelRequest>>,
    metadata: ModelContextMetadata,
    identity: Option<ProviderContextIdentity>,
    pending_dropped: Arc<AtomicBool>,
}

impl ScriptModel {
    fn new(attempts: impl IntoIterator<Item = Attempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            metadata: ModelContextMetadata::new("test", "overflow-model", 128_000).unwrap(),
            identity: None,
            pending_dropped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Model for ScriptModel {
    type Error = io::Error;

    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        panic!("runtime uses streaming")
    }

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        self.identity.as_ref()
    }

    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        error.kind() == io::ErrorKind::InvalidInput
    }

    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        self.requests.lock().unwrap().push(request);
        let attempt = self.attempts.lock().unwrap().pop_front().unwrap();
        match attempt {
            Attempt::Overflow | Attempt::PartialOverflow | Attempt::UsageOverflow => {
                match attempt {
                    Attempt::PartialOverflow => sink.emit(ModelStreamEvent::BlockStart {
                        index: 0,
                        kind: ModelBlockKind::ToolCall,
                    }),
                    Attempt::UsageOverflow => sink.emit(ModelStreamEvent::Usage {
                        usage: ModelUsage {
                            input_tokens: 100,
                            output_tokens: 0,
                            cached_input_tokens: None,
                        },
                    }),
                    _ => {}
                }
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "provider overflow",
                ))
            }
            Attempt::OtherError => Err(io::Error::other("context text is not a classification")),
            Attempt::Tool | Attempt::Final => {
                let response = if matches!(attempt, Attempt::Tool) {
                    ModelResponse::tool_calls(
                        "Writing".into(),
                        vec![ToolCall {
                            id: "write-1".into(),
                            name: "write".into(),
                            arguments: "{}".into(),
                        }],
                    )
                } else {
                    ModelResponse::final_answer("Done".into())
                };
                let mut output = ModelOutput::from(response);
                output.usage = Some(ModelUsage {
                    input_tokens: 42,
                    output_tokens: 8,
                    cached_input_tokens: Some(12),
                });
                output.emit_stream(sink);
                Ok(())
            }
            Attempt::Pending => {
                let _cleanup = DropFlag(Arc::clone(&self.pending_dropped));
                pending().await
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Recovery {
    Failure,
    Shrink,
    Unchanged,
    Grow,
    Invalid,
    None,
    Pending,
    Unmeasured,
}

struct RecoveringContext {
    recovery: Recovery,
    recoveries: AtomicUsize,
    checkpoint_written: AtomicBool,
    pending_dropped: Arc<AtomicBool>,
}

impl RecoveringContext {
    fn new(recovery: Recovery) -> Self {
        Self {
            recovery,
            recoveries: AtomicUsize::new(0),
            checkpoint_written: AtomicBool::new(false),
            pending_dropped: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ContextManager for RecoveringContext {
    type Error = io::Error;

    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
        if matches!(self.recovery, Recovery::Unmeasured) {
            None
        } else {
            Some(
                request
                    .messages
                    .iter()
                    .map(|message| message.text().len())
                    .sum(),
            )
        }
    }

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        Ok(PreparedContext {
            request: ModelRequest {
                instructions: input.instructions.into(),
                messages: vec![ModelMessage::user("large history ".repeat(100))],
                tools: input.tools.to_vec(),
                max_output_tokens: Some(100),
                provider_context: None,
            },
            omitted_history: None,
        })
    }

    async fn recover_overflow(
        &self,
        input: ContextInputRef<'_>,
        rejected: &ModelRequest,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        assert!(!matches!(
            input.events.last().map(|event| &event.event),
            Some(RunEvent::ModelRequestStarted)
        ));
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        if matches!(self.recovery, Recovery::Failure) {
            return Err(io::Error::other("summary storage unavailable"));
        }
        if matches!(self.recovery, Recovery::None) {
            return Ok(None);
        }
        if matches!(self.recovery, Recovery::Pending) {
            let _cleanup = DropFlag(Arc::clone(&self.pending_dropped));
            return pending().await;
        }
        let mut request = rejected.clone();
        match self.recovery {
            Recovery::Shrink | Recovery::Unmeasured => {
                request.messages = vec![ModelMessage::user("short summary".into())];
            }
            Recovery::Grow => request.messages.push(ModelMessage::user("extra".into())),
            Recovery::Invalid => {
                let mut message = ModelMessage::user(String::new());
                message.content = vec![ContentBlock::Reasoning { text: "bad".into() }];
                request.messages = vec![message];
            }
            _ => {}
        }
        self.checkpoint_written.store(true, Ordering::SeqCst);
        Ok(Some(PreparedContext {
            request,
            omitted_history: None,
        }))
    }
}

#[tokio::test]
async fn failed_recovery_retains_both_provider_and_compaction_errors() {
    let runtime = runtime([Attempt::Overflow], Recovery::Failure);
    let agent = agent();
    let mut run = agent.start_run("failed-recovery", "Finish");
    let error = runtime
        .run_until_blocked(&agent, &mut run)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentRuntimeError::ContextRecovery { .. }));
    let message = error.to_string();
    assert!(message.contains("provider overflow"));
    assert!(message.contains("summary storage unavailable"));
    assert!(
        matches!(&run.events().last().unwrap().event, RunEvent::RunFailed { message: stored } if stored == &message)
    );
    assert_eq!(runtime.model().calls(), 1);
}

struct Tools {
    calls: AtomicUsize,
    definitions: Vec<ToolDefinition>,
}

impl Tools {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            definitions: vec![ToolDefinition {
                name: "write".into(),
                description: "Write once".into(),
                input_schema: "{}".into(),
            }],
        }
    }
}

impl ToolExecutor for Tools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("written".into())
    }
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn agent() -> Agent {
    Agent::new("overflow-agent", "Overflow", "", "Follow the task")
}

type Runtime = AgentRuntime<ScriptModel, Tools, RecoveringContext>;
fn runtime(attempts: impl IntoIterator<Item = Attempt>, recovery: Recovery) -> Runtime {
    AgentRuntime::new(
        ScriptModel::new(attempts),
        Tools::new(),
        RecoveringContext::new(recovery),
        Some(5),
    )
}

#[tokio::test]
async fn a_confirmed_overflow_retries_a_smaller_context_inside_the_same_step() {
    let agent = agent();
    let runtime = runtime([Attempt::Overflow, Attempt::Final], Recovery::Shrink);
    let mut run = runtime
        .start_run(&agent, "recovered", "Finish")
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(runtime.model().calls(), 2);
    assert_eq!(
        runtime.context_manager().recoveries.load(Ordering::SeqCst),
        1
    );
    assert!(
        runtime
            .context_manager()
            .checkpoint_written
            .load(Ordering::SeqCst)
    );
    assert_eq!(run.current_step(), 1);
    assert_eq!(
        run.events()
            .iter()
            .filter(|event| matches!(event.event, RunEvent::ModelRequestStarted))
            .count(),
        1
    );
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event.event, RunEvent::RunFailed { .. }))
    );
    let requests = runtime.model().requests.lock().unwrap();
    assert_eq!(requests[1].messages[0].text(), "short summary");
    assert!(run.events().iter().any(|event| matches!(&event.event,
        RunEvent::ModelResponseReceived { token_usage: Some(record), .. }
        if record.usage.input_tokens == 42 && record.baseline.is_some()
    )));
}

#[tokio::test]
async fn durable_usage_binds_the_protocol_and_preserves_explicit_accounting_identity() {
    for explicit_identity in [None, Some("responses-with-careful-reasoning")] {
        let agent = agent();
        let mut model = ScriptModel::new([Attempt::Overflow, Attempt::Final]);
        let identity = ProviderContextIdentity {
            provider: "test".into(),
            model: "overflow-model".into(),
            format: "responses-v2".into(),
        };
        model.identity = Some(identity.clone());
        model.metadata.accounting_identity = explicit_identity.map(str::to_owned);
        let mut expected_metadata = model.metadata.clone();
        if expected_metadata.accounting_identity.is_none() {
            expected_metadata.accounting_identity = Some(
                serde_json::json!({
                    "provider": identity.provider,
                    "model": identity.model,
                    "format": identity.format,
                })
                .to_string(),
            );
        }
        let runtime = AgentRuntime::new(
            model,
            Tools::new(),
            RecoveringContext::new(Recovery::Shrink),
            Some(1),
        );
        let mut run = agent.start_run("protocol-binding", "Finish");
        assert_eq!(
            runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
            RunStatus::Completed
        );
        let requests = runtime.model().requests.lock().unwrap();
        let record = run
            .events()
            .iter()
            .find_map(|event| match &event.event {
                RunEvent::ModelResponseReceived { token_usage, .. } => token_usage.as_ref(),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            record.baseline,
            request_usage_baseline(Some(&expected_metadata), &requests[1])
        );
        assert_ne!(
            record.baseline,
            request_usage_baseline(Some(&expected_metadata), &requests[0]),
            "usage must bind the successful smaller request"
        );
    }
}

#[tokio::test]
async fn an_overflow_retry_cannot_recover_a_second_time() {
    let agent = agent();
    let runtime = runtime([Attempt::Overflow, Attempt::Overflow], Recovery::Shrink);
    let mut run = agent.start_run("bounded", "Finish");
    assert!(matches!(
        runtime.run_until_blocked(&agent, &mut run).await,
        Err(AgentRuntimeError::Model(_))
    ));
    assert_eq!(runtime.model().calls(), 2);
    assert_eq!(
        runtime.context_manager().recoveries.load(Ordering::SeqCst),
        1
    );
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(run.current_step(), 1);
}

#[tokio::test]
async fn partial_streams_usage_events_and_unclassified_errors_never_recover() {
    for attempt in [
        Attempt::PartialOverflow,
        Attempt::UsageOverflow,
        Attempt::OtherError,
    ] {
        let agent = agent();
        let runtime = runtime([attempt], Recovery::Shrink);
        let mut run = agent.start_run("no-recovery", "Finish");
        assert!(matches!(
            runtime.run_until_blocked(&agent, &mut run).await,
            Err(AgentRuntimeError::Model(_))
        ));
        assert_eq!(runtime.model().calls(), 1);
        assert_eq!(
            runtime.context_manager().recoveries.load(Ordering::SeqCst),
            0
        );
        assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 0);
        assert!(
            !run.events()
                .iter()
                .any(|event| matches!(event.event, RunEvent::ModelResponseReceived { .. }))
        );
    }
}

#[tokio::test]
async fn recovery_requires_a_provably_smaller_changed_request() {
    for recovery in [
        Recovery::Unchanged,
        Recovery::Grow,
        Recovery::None,
        Recovery::Unmeasured,
    ] {
        let agent = agent();
        let runtime = runtime([Attempt::Overflow], recovery);
        let mut run = agent.start_run("no-shrink", "Finish");
        let error = runtime
            .run_until_blocked(&agent, &mut run)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentRuntimeError::Model(error) if error.to_string() == "provider overflow")
        );
        assert_eq!(runtime.model().calls(), 1);
        assert_eq!(
            runtime.context_manager().recoveries.load(Ordering::SeqCst),
            1
        );
    }
}

#[tokio::test]
async fn a_recovery_candidate_must_pass_normal_protocol_validation() {
    let agent = agent();
    let runtime = runtime([Attempt::Overflow], Recovery::Invalid);
    let mut run = agent.start_run("invalid-recovery", "Finish");
    assert!(matches!(
        runtime.run_until_blocked(&agent, &mut run).await,
        Err(AgentRuntimeError::ModelProtocol(_))
    ));
    assert_eq!(runtime.model().calls(), 1);
    assert_eq!(run.status(), RunStatus::Failed);
}

#[tokio::test]
async fn recovery_never_reexecutes_previous_tool_effects() {
    let agent = agent();
    let runtime = runtime(
        [Attempt::Tool, Attempt::Overflow, Attempt::Final],
        Recovery::Shrink,
    );
    let mut run = agent.start_run("effects", "Write then finish");
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.model().calls(), 3);
    assert_eq!(run.current_step(), 2);
    assert_eq!(
        run.events()
            .iter()
            .filter(|event| matches!(event.event, RunEvent::ToolCallSucceeded { .. }))
            .count(),
        1
    );
}

async fn assert_cancellable(runtime: Runtime, dropped: Arc<AtomicBool>) {
    let agent = agent();
    let mut run = runtime
        .start_run(&agent, "cancel-recovery", "Finish")
        .await
        .unwrap();
    let mut cancel_handle = run.clone();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
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
    assert_eq!(run.status(), RunStatus::Cancelled);
    assert_eq!(run.current_step(), 1);
    assert!(
        !run.events()
            .iter()
            .any(|event| matches!(event.event, RunEvent::ModelResponseReceived { .. }))
    );
}

#[tokio::test]
async fn cancellation_drops_in_flight_compaction_before_retry() {
    let runtime = runtime([Attempt::Overflow], Recovery::Pending);
    let dropped = Arc::clone(&runtime.context_manager().pending_dropped);
    assert_cancellable(runtime, dropped).await;
}

#[tokio::test]
async fn cancellation_drops_the_retried_model_attempt() {
    let runtime = runtime([Attempt::Overflow, Attempt::Pending], Recovery::Shrink);
    let dropped = Arc::clone(&runtime.model().pending_dropped);
    assert_cancellable(runtime, dropped).await;
}
