use std::{
    convert::Infallible,
    future::{Future, pending},
    io,
    task::{Context, Poll, Waker},
};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ContextInputRef, ContextManager, Model, ModelFinishReason,
    ModelRequest, ModelResponse, PreparedContext, ToolCall, ToolDefinition, ToolExecutor,
};

struct Chat;
impl Model for Chat {
    type Error = Infallible;
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(ModelResponse::final_answer("Answer".into()))
    }
}
struct NoTools;
impl ToolExecutor for NoTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        unreachable!()
    }
}
#[derive(Clone, Copy)]
enum Compact {
    Pending,
    Failure,
    NoProgress,
}
impl ContextManager for Compact {
    type Error = io::Error;
    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        Ok(PreparedContext {
            request: ModelRequest {
                instructions: input.instructions.into(),
                messages: vec![],
                tools: vec![],
                max_output_tokens: None,
                provider_context: None,
            },
            omitted_history: None,
        })
    }
    async fn compact_now(
        &self,
        _: ContextInputRef<'_>,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        match self {
            Self::Pending => pending().await,
            Self::Failure => Err(io::Error::other("summary unavailable")),
            Self::NoProgress => Ok(None),
        }
    }
}
fn runtime(context: Compact) -> AgentRuntime<Chat, NoTools, Compact> {
    AgentRuntime::new(Chat, NoTools, context, Some(1)).with_continue_after_response(true)
}
fn agent() -> Agent {
    Agent::new("chat", "Chat", "", "")
}

#[tokio::test]
async fn conversational_mode_preserves_stop_and_accepts_followup_after_restore() {
    let runtime = runtime(Compact::NoProgress);
    let agent = agent();
    let mut run = runtime.start_run(&agent, "chat", "hello").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    assert!(run.events().iter().any(|event| matches!(&event.event,
        RunEvent::ModelResponseReceived { response: Some(response), .. } if response.finish_reason == ModelFinishReason::Stop)));
    let mut restored = runtime.restore_run("chat").await.unwrap().unwrap();
    runtime
        .submit_input(&mut restored, "continue")
        .await
        .unwrap();
    assert_eq!(
        runtime
            .run_until_blocked(&agent, &mut restored)
            .await
            .unwrap(),
        RunStatus::WaitingForInput
    );
    assert_eq!(restored.current_step(), 2);
}

#[tokio::test]
async fn manual_compaction_excludes_input_and_cancels_without_ending_conversation() {
    let runtime = runtime(Compact::Pending);
    let agent = agent();
    let mut run = runtime.start_run(&agent, "manual", "hello").await.unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    let before = run.events().to_vec();
    let mut input_handle = run.clone();
    let mut other_handle = run.clone();
    let mut maintenance = Box::pin(runtime.compact_now(&agent, &mut run));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(maintenance.as_mut().poll(&mut cx).is_pending());
    assert!(runtime.is_compacting("manual"));
    assert!(matches!(
        runtime.submit_input(&mut input_handle, "next").await,
        Err(AgentRuntimeError::RunAlreadyExecuting { .. })
    ));
    assert!(matches!(
        runtime.compact_now(&agent, &mut other_handle).await,
        Err(AgentRuntimeError::RunAlreadyExecuting { .. })
    ));
    let mut cancellation = Box::pin(runtime.cancel_compaction("manual"));
    assert!(cancellation.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        maintenance.as_mut().poll(&mut cx),
        Poll::Ready(Err(AgentRuntimeError::CompactionCancelled))
    ));
    assert!(matches!(
        cancellation.as_mut().poll(&mut cx),
        Poll::Ready(true)
    ));
    drop(maintenance);
    assert!(!runtime.is_compacting("manual"));
    let restored = runtime.restore_run("manual").await.unwrap().unwrap();
    assert_eq!(restored.status(), RunStatus::WaitingForInput);
    assert_eq!(restored.events(), before);
    runtime.submit_input(&mut run, "continue").await.unwrap();
}

#[tokio::test]
async fn compaction_failure_and_no_progress_leave_history_and_status_unchanged() {
    for mode in [Compact::Failure, Compact::NoProgress] {
        let runtime = runtime(mode);
        let agent = agent();
        let mut run = runtime.start_run(&agent, "idle", "hello").await.unwrap();
        runtime.run_until_blocked(&agent, &mut run).await.unwrap();
        let before = run.events().to_vec();
        let result = runtime.compact_now(&agent, &mut run).await;
        match mode {
            Compact::Failure => assert!(matches!(result, Err(AgentRuntimeError::Context(_)))),
            Compact::NoProgress => assert!(!result.unwrap()),
            Compact::Pending => unreachable!(),
        }
        assert_eq!(run.events(), before);
        assert_eq!(run.status(), RunStatus::WaitingForInput);
        assert!(!runtime.is_executing("idle"));
    }
}

#[tokio::test]
async fn unsettled_runs_cannot_be_manually_compacted() {
    let runtime = runtime(Compact::Pending);
    let agent = agent();
    let mut run = runtime.start_run(&agent, "new", "hello").await.unwrap();
    assert!(matches!(
        runtime.compact_now(&agent, &mut run).await,
        Err(AgentRuntimeError::RunNotIdle { .. })
    ));
    assert!(!runtime.is_executing("new"));
}
