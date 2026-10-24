#[path = "support/scripted_model.rs"]
mod scripted_model;
mod support;

use std::{
    convert::Infallible,
    sync::atomic::{AtomicUsize, Ordering},
};

use agent_core::agent::{Agent, AgentRun, RunEvent, RunEventEnvelope, RunPauseReason, RunStatus};
use agent_runtime::{
    AgentRuntime, ApproximateTokenCounter, CompactingContextManager, ContextCompactor,
    ContextPolicy, ContextSummary, InMemoryContextCheckpointStore, ModelResponse, TokenCounter,
    ToolCall, ToolDefinition, ToolExecutor,
};
use scripted_model::ScriptedModel;
use support::test_context_manager;

struct PollTools {
    definitions: Vec<ToolDefinition>,
    calls: AtomicUsize,
}

impl PollTools {
    fn new() -> Self {
        Self {
            definitions: vec![ToolDefinition {
                name: "poll".into(),
                description: "Read changing job progress".into(),
                input_schema: "{}".into(),
            }],
            calls: AtomicUsize::new(0),
        }
    }
}

impl ToolExecutor for PollTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        let progress = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("Progress {progress}: {}", "record ".repeat(120)))
    }
}

fn agent() -> Agent {
    Agent::new("worker", "Worker", "", "Finish the task.")
}

fn poll(index: u32, arguments: &str) -> ModelResponse {
    ModelResponse::tool_calls(
        "Check progress".into(),
        vec![ToolCall {
            id: format!("poll-{index}"),
            name: "poll".into(),
            arguments: arguments.into(),
        }],
    )
}

fn warnings(run: &AgentRun) -> Vec<u32> {
    run.events()
        .iter()
        .filter_map(|entry| match &entry.event {
            RunEvent::ToolLoopWarning {
                consecutive_calls, ..
            } => Some(*consecutive_calls),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn unlimited_execution_finishes_after_more_than_twelve_steps() {
    let agent = agent();
    let mut run = agent.start_run("unlimited", "Work");
    let runtime = AgentRuntime::new(
        ScriptedModel::new(
            (1..=15)
                .map(|index| poll(index, "{}"))
                .chain([ModelResponse::final_answer("Done".into())]),
        ),
        PollTools::new(),
        test_context_manager(),
        None,
    );
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(run.current_step(), 16);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 15);
    assert_eq!(warnings(&run), vec![3, 5, 8]);
    let requests = runtime.model().requests();
    assert!(
        requests[3]
            .messages
            .iter()
            .any(|message| message.text().contains("3 consecutive times"))
    );
    assert!(
        requests[5]
            .messages
            .iter()
            .any(|message| message.text().contains("5 consecutive times"))
    );
    assert!(
        requests[8]
            .messages
            .iter()
            .any(|message| message.text().contains("8 consecutive times"))
    );
}

#[tokio::test]
async fn a_paused_budget_is_durable_and_resume_starts_a_fresh_budget() {
    let agent = agent();
    let runtime = AgentRuntime::new(
        ScriptedModel::new([
            poll(1, "{}"),
            poll(2, "{}"),
            ModelResponse::final_answer("Done".into()),
        ]),
        PollTools::new(),
        test_context_manager(),
        Some(2),
    );
    let mut run = runtime.start_run(&agent, "limited", "Work").await.unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Paused
    );
    assert!(matches!(
        run.events().last().unwrap().event,
        RunEvent::RunPaused {
            reason: RunPauseReason::StepLimit { max_steps: 2 }
        }
    ));
    assert!(
        !run.events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::RunFailed { .. }))
    );
    let mut restored = runtime.restore_run("limited").await.unwrap().unwrap();
    assert_eq!(restored.status(), RunStatus::Paused);
    assert!(!restored.can_begin_step());
    runtime.resume_run(&mut restored, "Continue").await.unwrap();
    assert_eq!(
        runtime
            .run_until_blocked(&agent, &mut restored)
            .await
            .unwrap(),
        RunStatus::Completed
    );
    assert_eq!(restored.current_step(), 3);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.model().requests()[2].messages.iter().flat_map(|message| &message.content).filter(|block| matches!(block, agent_core::ContentBlock::ToolResult { content, .. } if content.contains("Progress"))).count(), 2);
}

#[tokio::test]
async fn repetitions_normalize_json_and_reset_on_new_user_input() {
    let agent = agent();
    let mut run = agent.start_run("user-reset", "Work");
    let runtime = AgentRuntime::new(
        ScriptedModel::new([
            poll(1, r#"{"a":1,"b":2}"#),
            poll(2, r#"{ "b":2, "a":1 }"#),
            poll(3, r#"{"a":1,"b":2}"#),
            ModelResponse::input_required("Continue polling?".into()),
            poll(4, r#"{"a":1,"b":2}"#),
            poll(5, r#"{"a":1,"b":2}"#),
            poll(6, r#"{"a":1,"b":2}"#),
            ModelResponse::final_answer("Done".into()),
        ]),
        PollTools::new(),
        test_context_manager(),
        None,
    );
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    runtime.submit_input(&mut run, "Yes").await.unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(warnings(&run), vec![3, 3]);
}

#[tokio::test]
async fn answering_an_input_request_resets_the_budget_for_non_interactive_agents() {
    let agent = agent();
    let runtime = AgentRuntime::new(
        ScriptedModel::new([
            poll(1, "{}"),
            ModelResponse::input_required("Which result should I inspect?".into()),
            poll(2, "{}"),
            ModelResponse::final_answer("Inspected the requested result".into()),
        ]),
        PollTools::new(),
        test_context_manager(),
        Some(2),
    );
    let mut run = runtime
        .start_run(&agent, "input-budget", "Work")
        .await
        .unwrap();
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::WaitingForInput
    );
    assert_eq!(run.current_step(), 2);
    runtime
        .submit_input(&mut run, "Inspect the latest result")
        .await
        .unwrap();
    let mut restored = runtime.restore_run("input-budget").await.unwrap().unwrap();
    assert_eq!(
        runtime
            .run_until_blocked(&agent, &mut restored)
            .await
            .unwrap(),
        RunStatus::Completed
    );
    assert_eq!(restored.current_step(), 4);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 2);
    assert!(
        !restored
            .events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::RunPaused { .. }))
    );
}

#[tokio::test]
async fn explicit_resume_resets_repetition_counts() {
    let agent = agent();
    let mut run = agent.start_run("resume-reset", "Work");
    let runtime = AgentRuntime::new(
        ScriptedModel::new((1..=6).map(|index| poll(index, "{}"))),
        PollTools::new(),
        test_context_manager(),
        Some(3),
    );
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    runtime.resume_run(&mut run, "Continue").await.unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(warnings(&run), vec![3, 3]);
}

#[tokio::test]
async fn changed_arguments_break_the_consecutive_streak() {
    let agent = agent();
    let mut run = agent.start_run("different-input", "Work");
    let runtime = AgentRuntime::new(
        ScriptedModel::new([
            poll(1, "{}"),
            poll(2, "{}"),
            poll(3, r#"{"job":2}"#),
            poll(4, "{}"),
            poll(5, "{}"),
            ModelResponse::final_answer("Done".into()),
        ]),
        PollTools::new(),
        test_context_manager(),
        None,
    );
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert!(warnings(&run).is_empty());
}

#[tokio::test]
async fn restoring_a_safe_boundary_rebuilds_counts_without_duplicate_warnings() {
    let agent = agent();
    let mut history = agent.start_run("restored", "Work");
    for index in 1..=4 {
        history.begin_step().unwrap();
        let response = poll(index, "{}");
        history
            .record(RunEvent::ModelResponseReceived {
                content: response.text(),
                response: Some(response),
                token_usage: None,
                provider_state: None,
                provider_continuation: None,
            })
            .unwrap();
        history
            .record(RunEvent::ToolCallRequested {
                call_id: format!("poll-{index}"),
                tool_name: "poll".into(),
                arguments: "{}".into(),
            })
            .unwrap();
        history
            .record(RunEvent::ToolCallSucceeded {
                call_id: format!("poll-{index}"),
                output: "Progress".into(),
                images: vec![],
            })
            .unwrap();
        if index == 3 {
            history
                .record(RunEvent::ToolLoopWarning {
                    call_id: "poll-3".into(),
                    tool_name: "poll".into(),
                    consecutive_calls: 3,
                })
                .unwrap();
        }
    }
    let mut restored = AgentRun::restore("restored", "worker", history.events().to_vec()).unwrap();
    let runtime = AgentRuntime::new(
        ScriptedModel::new(
            (5..=8)
                .map(|index| poll(index, "{}"))
                .chain([ModelResponse::final_answer("Done".into())]),
        ),
        PollTools::new(),
        test_context_manager(),
        None,
    );
    runtime
        .run_until_blocked(&agent, &mut restored)
        .await
        .unwrap();
    assert_eq!(warnings(&restored), vec![3, 5, 8]);
}

#[derive(Default)]
struct CompactHistory(AtomicUsize);
impl ContextCompactor for CompactHistory {
    type Error = Infallible;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        _: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ContextSummary {
            objective: "Finish polling".into(),
            ..ContextSummary::default()
        })
    }
}

#[tokio::test]
async fn compaction_does_not_reset_the_repetition_monitor() {
    let agent = agent();
    let mut run = agent.start_run("compacted", "Work");
    let manager = CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(6_000)
            .compaction_threshold(4_000)
            .compaction_target(2_500)
            .recent_tail_tokens(1_500)
            .max_tool_output_chars(2_000)
            .build()
            .unwrap(),
        CompactHistory::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let runtime = AgentRuntime::new(
        ScriptedModel::new(
            (1..=8)
                .map(|index| poll(index, "{}"))
                .chain([ModelResponse::final_answer("Done".into())]),
        ),
        PollTools::new(),
        manager,
        None,
    );
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(warnings(&run), vec![3, 5, 8]);
    assert!(
        runtime
            .context_manager()
            .compactor()
            .0
            .load(Ordering::SeqCst)
            > 0
    );
}
