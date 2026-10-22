//! Opt-in live evaluation. See examples/deepseek_context/README.md.
#[path = "deepseek_context/accounting.rs"]
mod accounting;
#[path = "deepseek_context/failure.rs"]
mod failure;
#[path = "deepseek_context/model.rs"]
mod model;
#[path = "deepseek_context/precision.rs"]
mod precision;
#[path = "deepseek_context/scenarios.rs"]
mod scenarios;

use agent_runtime::{ContextPolicy, Model, ModelMessage, ModelRequest};
use serde_json::{Value, json};

type EvalError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), EvalError> {
    let scenario = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if !matches!(
        scenario.as_str(),
        "all" | "large" | "runtime" | "failure" | "probe" | "accounting" | "precision"
    ) {
        return Err("usage: deepseek_context [all|large|runtime|failure|probe|accounting|precision] [non-thinking|thinking|thinking-drop]".into());
    }
    let mode = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "non-thinking".into());
    if !matches!(mode.as_str(), "non-thinking" | "thinking" | "thinking-drop")
        || (mode == "thinking-drop" && scenario != "runtime")
    {
        return Err("invalid mode; thinking-drop is only for the runtime negative control".into());
    }
    let thinking = mode != "non-thinking";
    let model = model::DeepSeekModel::discover().await?;
    let model = if thinking {
        model.with_thinking(mode != "thinking-drop")
    } else {
        model
    };
    let policy = ContextPolicy::default().resolve(model.context_metadata())?;
    let mut report = json!({
        "model":model.catalog(),"mode":mode,"scenario":scenario,
        "reasoning_effort":if thinking {Some("high")} else {None},
        "requested_revision":"DeepSeek-V4.1-Flash-1017",
        "started_at_unix_seconds":std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        "resolved_default_policy":{
            "window":policy.model_budget().context_window(),
            "input_limit":policy.model_budget().input_token_limit(),
            "threshold":policy.compaction_threshold(),"target":policy.compaction_target(),
            "recent_tail":policy.recent_tail_tokens(),
        },
        "limitations":[
            "Synthetic fixtures; not a production reliability or model-quality benchmark.",
            "The compaction scenario explicitly lowers thresholds to exercise repeated compression.",
            "Provider-native compaction is not covered. Thinking replay uses durable per-response state; separate offline tests cover restart and other protocols.",
            "No transport retry; errors are recorded rather than masked."
        ]
    });
    eprintln!(
        "catalog={} policy={}",
        report["model"], report["resolved_default_policy"]
    );
    if scenario == "probe" {
        let output = model
            .respond_with_metadata(ModelRequest {
                instructions: "Reply with exactly OK.".into(),
                messages: vec![ModelMessage::user("Connectivity probe.".into())],
                tools: vec![],
                max_output_tokens: Some(if thinking { 8192 } else { 16 }),
                provider_context: None,
            })
            .await?;
        report["probe"] = json!({"response":format!("{:?}",output.response)});
    }
    if scenario == "all" || scenario == "large" {
        if thinking {
            report["large"] =
                capture(scenarios::run_thinking_window_scenario(model.clone(), false).await);
            report["near_window"] =
                capture(scenarios::run_thinking_window_scenario(model.clone(), true).await);
        } else {
            report["large"] = capture(scenarios::run_large_window_scenario(model.clone()).await);
            report["near_window"] =
                capture(scenarios::run_near_window_scenario(model.clone()).await);
        }
    }
    if scenario == "all" || scenario == "runtime" {
        let result = if thinking {
            scenarios::run_runtime_with_counter(model.clone(), model.token_counter(), 8192, 8192)
                .await
        } else {
            scenarios::run_runtime_with_counter(model.clone(), model.token_counter(), 1024, 2048)
                .await
        };
        let result = capture(result);
        if mode == "thinking-drop" {
            let error = result["runtime_error"].as_str().unwrap_or_default();
            report["negative_control"] = json!({
                "passed":result["main_calls"].as_array().is_some_and(|calls| calls.len() >= 2),
                "scope":"Diagnostic control: observe server behavior without assuming it always enforces the documented replay requirement.",
                "provider_rejected_missing_reasoning":error.contains("400") && error.contains("reasoning"),
                "runtime_completed_without_replay":result["passed"],
                "runtime_diagnostic":result,
            });
        } else {
            report["runtime"] = result;
        }
    }
    if scenario == "all" || scenario == "failure" {
        report["failure"] = capture(failure::run(model.clone()).await);
    }
    if thinking && mode != "thinking-drop" && (scenario == "all" || scenario == "accounting") {
        report["accounting"] = capture(accounting::run(model.clone()).await);
    }
    // Explicit-only to keep the existing all-scenarios request budget bounded.
    if scenario == "precision" {
        let precision_model = model
            .clone()
            .with_output_cap(if thinking { 8192 } else { 256 });
        report["precision"] =
            capture(precision::run(precision_model.clone(), precision_model.token_counter()).await);
    }
    report["requests"] = json!(model.observations());
    if thinking {
        report["thinking_verified"] = json!({
            "passed":model.observations().iter().any(|observation|
                observation["usage"]["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0) > 0),
            "criterion":"At least one provider response reports nonzero reasoning_tokens."
        });
    }
    let output_dir = std::path::Path::new("evals/results");
    std::fs::create_dir_all(output_dir)?;
    let label = std::env::args().nth(3).unwrap_or_default();
    if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("report label must be alphanumeric or hyphens".into());
    }
    let suffix = if label.is_empty() {
        String::new()
    } else {
        format!("-{label}")
    };
    let path = output_dir.join(if thinking {
        format!("deepseek-context-{mode}-{scenario}{suffix}.json")
    } else {
        format!("deepseek-context-{scenario}{suffix}.json")
    });
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    println!("report={}", path.display());
    let checks = [
        "large",
        "near_window",
        "runtime",
        "failure",
        "accounting",
        "precision",
        "negative_control",
        "thinking_verified",
    ];
    for name in checks {
        if let Some(result) = report.get(name) {
            println!(
                "{name}: passed={}, error={}",
                result["passed"], result["error"]
            );
        }
    }
    if checks.iter().any(|name| {
        report
            .get(name)
            .is_some_and(|result| result["passed"] != true)
    }) {
        return Err("one or more live checks failed; inspect the saved report".into());
    }
    Ok(())
}

fn capture(result: Result<Value, EvalError>) -> Value {
    result.unwrap_or_else(|error| json!({"passed":false,"error":error.to_string()}))
}
