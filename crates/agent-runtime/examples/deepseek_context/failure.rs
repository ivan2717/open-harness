use agent_core::agent::{Agent, RunEvent};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCheckpointStore, ContextInput,
    ContextManager, ContextPolicy, InMemoryContextCheckpointStore, Model, ModelContextBudget,
    ModelContextCompactor,
};
use serde_json::{Value, json};

use super::model::DeepSeekModel;

pub async fn run(model: DeepSeekModel) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let agent = Agent::new(
        "failure-eval",
        "Failure evaluation",
        "",
        "Preserve exact identifiers.",
    );
    let mut run = agent.start_run(
        "summary-failure",
        "Keep ticket TICKET-8821 and retry only after approval.",
    );
    for i in 0..3 {
        run.begin_step()?;
        run.record(RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: String::new(),
        })?;
        run.record(RunEvent::ToolCallRequested {
            call_id: format!("call-{i}"),
            tool_name: "inspect".into(),
            arguments: "{}".into(),
        })?;
        run.record(RunEvent::ToolCallSucceeded {
            call_id: format!("call-{i}"),
            output: format!(
                "Batch {i}: important artifact /work/result-{i}.json\n{}",
                "unchanged log entry\n".repeat(500)
            ),
            images: Vec::new(),
        })?;
    }
    let events = run.events().to_vec();
    let metadata = model.context_metadata().unwrap().clone();
    let input = ContextInput::new(run.id(), &agent.instructions, events.clone(), vec![])
        .with_model_metadata(metadata.clone());
    let mut results = Vec::new();
    for (name, limit) in [("soft", 20_000), ("hard", 3_000)] {
        let checkpoints = InMemoryContextCheckpointStore::new();
        let policy = ContextPolicy::builder()
            .input_token_limit(limit)
            .compaction_threshold(2_000)
            .compaction_target(1_200)
            .recent_tail_tokens(100)
            .build_for_model(&metadata)?;
        let compactor = ModelContextCompactor::new(model.with_output_cap(32))
            .with_budget(ModelContextBudget::new(metadata.context_window, 32, 2048)?);
        let manager = CompactingContextManager::new(
            policy,
            compactor,
            checkpoints.clone(),
            ApproximateTokenCounter::default(),
        );
        let prepared = manager.prepare(input.as_ref()).await;
        let no_checkpoint = checkpoints.latest(run.id().to_owned()).await?.is_none();
        let behavior_ok = if name == "soft" {
            prepared
                .as_ref()
                .is_ok_and(|context| context.omitted_history.is_none())
        } else {
            prepared.is_err()
        };
        results.push(json!({
            "case":name,"input_token_limit":limit,"summary_output_cap":32,
            "prepared":prepared.is_ok(),"error":prepared.err().map(|error|error.to_string()),
            "no_checkpoint_saved":no_checkpoint,"original_events_unchanged":run.events()==events,
            "passed":behavior_ok && no_checkpoint && run.events()==events,
        }));
    }
    Ok(json!({"cases":results,"passed":results.iter().all(|case|case["passed"]==true)}))
}
