#[path = "support/scripted_model.rs"]
mod scripted_model;
mod support;

use agent_core::agent::{Agent, AgentRun, RunEvent};
use agent_runtime::{AgentRuntime, ModelResponse, ToolCall, ToolDefinition, ToolExecutor};
use scripted_model::ScriptedModel;
use serde_json::{Value, json};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};
use support::test_context_manager;

struct WaitTools {
    definitions: Vec<ToolDefinition>,
    outputs: Vec<Option<Value>>,
    next: AtomicUsize,
}

impl ToolExecutor for WaitTools {
    type Error = io::Error;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn execute(&self, _: ToolCall) -> io::Result<String> {
        self.outputs[self.next.fetch_add(1, Ordering::SeqCst)]
            .as_ref()
            .map(Value::to_string)
            .ok_or_else(|| io::Error::other("collaboration wait failed"))
    }
}

fn result(outcome: &str, token: &str, waited_ms: u64) -> Value {
    json!({
        "members": [{"agent_id": "child", "status": "running"}],
        "wait": {"outcome": outcome, "progress_token": token, "waited_ms": waited_ms},
    })
}

fn tool_call(index: usize, name: &str, arguments: &Value) -> ModelResponse {
    ModelResponse::tool_calls(
        String::new(),
        vec![ToolCall {
            id: format!("call-{index}"),
            name: name.into(),
            arguments: arguments.to_string(),
        }],
    )
}

fn warnings(run: &AgentRun) -> Vec<u32> {
    run.events()
        .iter()
        .filter_map(|entry| match entry.event {
            RunEvent::ToolLoopWarning {
                consecutive_calls, ..
            } => Some(consecutive_calls),
            _ => None,
        })
        .collect()
}

fn runtime(
    name: &str,
    arguments: &Value,
    first_call: usize,
    outputs: Vec<Option<Value>>,
) -> AgentRuntime<ScriptedModel, WaitTools, impl agent_runtime::ContextManager> {
    AgentRuntime::new(
        ScriptedModel::new(
            (first_call..first_call + outputs.len())
                .map(|index| tool_call(index, name, arguments))
                .chain([ModelResponse::final_answer("Done".into())]),
        ),
        WaitTools {
            definitions: vec![ToolDefinition {
                name: name.into(),
                description: String::new(),
                input_schema: "{}".into(),
            }],
            outputs,
            next: AtomicUsize::new(0),
        },
        test_context_manager(),
        None,
    )
}

async fn run(name: &str, arguments: Value, outputs: Vec<Option<Value>>) -> AgentRun {
    let agent = Agent::new("coordinator", "Coordinator", "", "Finish the task.");
    let expected_steps = outputs.len() + 1;
    let runtime = runtime(name, &arguments, 0, outputs);
    let mut run = agent.start_run("collaboration-waiting", "Work");
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(runtime.model().requests().len(), expected_steps);
    run
}

#[tokio::test]
async fn new_agent_messages_and_status_activity_do_not_emit_loop_warnings() {
    for outcome in ["message", "activity"] {
        let run = run(
            "agent_control",
            json!({"action": "wait", "timeout_seconds": 0}),
            (0..9)
                .map(|index| Some(result(outcome, &format!("activity-{index}"), 0)))
                .collect(),
        )
        .await;
        assert!(warnings(&run).is_empty(), "outcome: {outcome}");
    }
}

#[tokio::test]
async fn real_blocking_waits_without_activity_do_not_emit_loop_warnings() {
    for arguments in [
        json!({"action": "wait"}),
        json!({"action": "wait", "timeout_seconds": 1}),
        json!({"action": "wait", "target": "reader", "timeout_seconds": 1}),
        json!({"action": "wait", "agent_id": "child", "timeout_seconds": 1}),
    ] {
        let run = run(
            "agent_control",
            arguments,
            vec![Some(result("timeout", "unchanged", 1000)); 9],
        )
        .await;
        assert!(warnings(&run).is_empty());
    }
}

#[tokio::test]
async fn targeted_waits_require_new_activity_to_reset_repetition() {
    for key in ["target", "agent_id"] {
        let mut arguments = json!({"action": "wait", "timeout_seconds": 0});
        arguments[key] = json!("reader");
        let progressing = run(
            "agent_control",
            arguments.clone(),
            (0..9)
                .map(|index| Some(result("activity", &format!("reader-{index}"), 0)))
                .collect(),
        )
        .await;
        assert!(warnings(&progressing).is_empty());
        let unchanged = run(
            "agent_control",
            arguments,
            vec![Some(result("activity", "same-reader-state", 0)); 9],
        )
        .await;
        assert_eq!(warnings(&unchanged), vec![3, 5, 8]);
    }
}

#[tokio::test]
async fn rereading_the_same_message_warns_even_when_observed_usage_changes() {
    for outcome in ["message", "activity"] {
        let run = run(
            "agent_control",
            json!({"action": "wait", "timeout_seconds": 30}),
            (0..9)
                .map(|index| {
                    let mut output = result(outcome, "same-message", 0);
                    output["members"][0]["observed_usage"] = json!(index * 1000);
                    Some(output)
                })
                .collect(),
        )
        .await;
        // The first observation is useful; the next eight read the same event.
        assert_eq!(warnings(&run), vec![3, 5, 8], "outcome: {outcome}");
    }
}

#[tokio::test]
async fn no_progress_and_short_or_failed_waits_still_warn() {
    for output in [
        Some(result("no_progress", "unchanged", 30_000)),
        Some(result("timeout", "unchanged", 0)),
        Some(result("timeout", "unchanged", 999)),
        Some(result("unexpected", "unchanged", 1000)),
        Some(result("message", "", 1000)),
        Some(json!({"members": [{"status": "running"}]})),
        None,
    ] {
        let run = run(
            "agent_control",
            json!({"action": "wait", "timeout_seconds": 30}),
            vec![output; 8],
        )
        .await;
        assert_eq!(warnings(&run), vec![3, 5, 8]);
    }
    let run = run(
        "agent_control",
        json!({"action": "wait", "timeout_seconds": 0}),
        vec![Some(result("timeout", "unchanged", 1000)); 8],
    )
    .await;
    assert_eq!(warnings(&run), vec![3, 5, 8]);
}

#[tokio::test]
async fn no_progress_is_not_mistaken_for_new_activity() {
    let run = run(
        "agent_control",
        json!({"action": "wait"}),
        (0..8)
            .map(|index| Some(result("no_progress", &format!("idle-{index}"), 0)))
            .collect(),
    )
    .await;
    assert_eq!(warnings(&run), vec![3, 5, 8]);
}

#[tokio::test]
async fn wait_progress_does_not_exempt_other_actions_tools_or_invalid_inputs() {
    for (name, arguments) in [
        ("agent_control", json!({"action": "list"})),
        ("other_wait", json!({"action": "wait"})),
        (
            "agent_control",
            json!({"action": "wait", "timeout_seconds": 31}),
        ),
        (
            "agent_control",
            json!({"action": "wait", "timeout_seconds": -1}),
        ),
        (
            "agent_control",
            json!({"action": "wait", "timeout_seconds": "30"}),
        ),
        (
            "agent_control",
            json!({"action": "wait", "message": "extra"}),
        ),
        (
            "agent_control",
            json!({"action": "wait", "target": "reader", "include_tools": true}),
        ),
        (
            "agent_control",
            json!({"action": "wait", "target": "reader", "agent_id": "reader"}),
        ),
        ("agent_control", json!({"action": "wait", "target": ""})),
        ("agent_control", json!({"action": "wait", "agent_id": "  "})),
        ("agent_control", json!({"action": "wait", "target": 1})),
        ("agent_control", json!({"action": "wait", "agent_id": null})),
    ] {
        let run = run(
            name,
            arguments,
            vec![Some(result("timeout", "unchanged", 1000)); 8],
        )
        .await;
        assert_eq!(warnings(&run), vec![3, 5, 8]);
    }
}

#[tokio::test]
async fn restored_tool_history_preserves_wait_activity_classification() {
    for (output, expected) in [
        (result("timeout", "unchanged", 1000), vec![]),
        (result("message", "already-read", 0), vec![3, 5, 8]),
    ] {
        let arguments = json!({"action": "wait"});
        let original = run(
            "agent_control",
            arguments.clone(),
            vec![Some(output.clone()); 4],
        )
        .await;
        // Restore a safe tool boundary, without a new user turn resetting the
        // repetition state. Compacted model context is not used for this check.
        let events = original
            .events()
            .iter()
            .take_while(|entry| entry.step != 5)
            .cloned()
            .collect();
        let mut restored =
            AgentRun::restore("collaboration-waiting", "coordinator", events).unwrap();
        let agent = Agent::new("coordinator", "Coordinator", "", "Finish the task.");
        runtime("agent_control", &arguments, 4, vec![Some(output); 5])
            .run_until_blocked(&agent, &mut restored)
            .await
            .unwrap();
        assert_eq!(warnings(&restored), expected);
    }
}
