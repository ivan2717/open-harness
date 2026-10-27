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

struct CommandTools {
    definitions: Vec<ToolDefinition>,
    output: Vec<Option<Value>>,
    next: AtomicUsize,
}
impl ToolExecutor for CommandTools {
    type Error = io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> io::Result<String> {
        self.output[self.next.fetch_add(1, Ordering::SeqCst)]
            .as_ref()
            .map(Value::to_string)
            .ok_or_else(|| io::Error::other("command session not found"))
    }
}
fn result(status: &str, next: u64) -> Value {
    json!({"command_id":"command-1","status":status,"exit_code":null,
        "output":if next == 0 { "" } else { "output" },"next_cursor":next,
        "output_truncated":false,"has_more_output":false})
}
fn call(index: usize, name: &str, arguments: &Value) -> ModelResponse {
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
async fn run(name: &str, arguments: Value, output: Vec<Option<Value>>) -> AgentRun {
    let agent = Agent::new("worker", "Worker", "", "Finish the task.");
    let expected_steps = output.len() + 1;
    let runtime = AgentRuntime::new(
        ScriptedModel::new(
            (0..output.len())
                .map(|index| call(index, name, &arguments))
                .chain([ModelResponse::final_answer("Done".into())]),
        ),
        CommandTools {
            definitions: vec![ToolDefinition {
                name: name.into(),
                description: String::new(),
                input_schema: "{}".into(),
            }],
            output,
            next: AtomicUsize::new(0),
        },
        test_context_manager(),
        None,
    );
    let mut run = agent.start_run("command-polling", "Work");
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    assert_eq!(runtime.model().requests().len(), expected_steps);
    run
}

#[tokio::test]
async fn quiet_live_command_waits_do_not_emit_false_loop_warnings() {
    let run = run(
        "manage_command",
        json!({"command_id":"command-1","action":"output","cursor":0,"wait_ms":1000}),
        vec![Some(result("running", 0)); 9],
    )
    .await;
    assert!(warnings(&run).is_empty());
}

#[tokio::test]
async fn new_command_output_breaks_the_repetition_streak_but_old_pages_do_not() {
    let arguments = json!({"command_id":"command-1","action":"status","cursor":0,"wait_ms":1000});
    let progressing = run(
        "manage_command",
        arguments.clone(),
        (1..=9).map(|next| Some(result("running", next))).collect(),
    )
    .await;
    assert!(warnings(&progressing).is_empty());
    let stale = run(
        "manage_command",
        arguments,
        vec![Some(result("running", 10)); 9],
    )
    .await;
    // The first observation is progress; repeatedly reading it is not a wait,
    // even when the request advertises wait_ms, because output was already ready.
    assert_eq!(warnings(&stale), vec![3, 5, 8]);
}

#[tokio::test]
async fn zero_wait_completed_failed_stop_and_mismatched_reads_still_warn() {
    let args = json!({"command_id":"command-1","wait_ms":0});
    assert_eq!(
        warnings(&run("manage_command", args, vec![Some(result("running", 0)); 8]).await),
        vec![3, 5, 8]
    );
    let args = json!({"command_id":"command-1","wait_ms":1000});
    let mut mismatched = result("running", 0);
    mismatched["command_id"] = json!("different");
    for output in [
        Some(result("completed", 0)),
        None,
        Some(json!({"status":"running"})),
        Some(mismatched),
    ] {
        assert_eq!(
            warnings(&run("manage_command", args.clone(), vec![output; 8]).await),
            vec![3, 5, 8]
        );
    }
    let stop = json!({"command_id":"command-1","action":"stop","wait_ms":1000});
    assert_eq!(
        warnings(&run("manage_command", stop, vec![Some(result("running", 0)); 8]).await),
        vec![3, 5, 8]
    );
    // A similarly shaped response from a different tool is ordinary repetition.
    assert_eq!(
        warnings(&run("other_poll", args, vec![Some(result("running", 0)); 8]).await),
        vec![3, 5, 8]
    );
}

#[tokio::test]
async fn restoring_history_preserves_live_wait_and_stale_page_classification() {
    for (next_cursor, expected_warnings) in [(0, vec![]), (10, vec![3, 5, 8])] {
        let mut history = run(
            "manage_command",
            json!({"command_id":"command-1","wait_ms":1000}),
            vec![Some(result("running", next_cursor)); 4],
        )
        .await;
        let agent = Agent::new("worker", "Worker", "", "Finish the task.");
        // Resume is a new user turn and intentionally resets repetition counts.
        // Instead restore the safe tool boundary immediately before the final answer.
        let events: Vec<_> = history
            .events()
            .iter()
            .take_while(|entry| entry.step != 5)
            .cloned()
            .collect();
        history = AgentRun::restore("command-polling", "worker", events).unwrap();
        let arguments = json!({"command_id":"command-1","wait_ms":1000});
        let runtime = AgentRuntime::new(
            ScriptedModel::new(
                (4..9)
                    .map(|index| call(index, "manage_command", &arguments))
                    .chain([ModelResponse::final_answer("Done".into())]),
            ),
            CommandTools {
                definitions: vec![ToolDefinition {
                    name: "manage_command".into(),
                    description: String::new(),
                    input_schema: "{}".into(),
                }],
                output: vec![Some(result("running", next_cursor)); 5],
                next: AtomicUsize::new(0),
            },
            test_context_manager(),
            None,
        );
        runtime
            .run_until_blocked(&agent, &mut history)
            .await
            .unwrap();
        assert_eq!(warnings(&history), expected_warnings);
    }
}
