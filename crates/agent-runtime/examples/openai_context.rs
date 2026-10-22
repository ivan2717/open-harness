//! Opt-in GPT-6 Luna evaluation. See examples/openai_context/README.md.
#[path = "openai_context/model.rs"]
mod model;
#[path = "deepseek_context/precision.rs"]
mod precision;
// Reuse the exact synthetic fixtures; this entry point runs only the bounded
// runtime scenario, not the large/near-window DeepSeek fixtures in this module.
#[allow(dead_code)]
#[path = "deepseek_context/scenarios.rs"]
mod scenarios;

use agent_runtime::{ContextPolicy, Model, ModelMessage, ModelRequest};
use serde_json::{Value, json};

type EvalError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), EvalError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let scenario = args.first().map(String::as_str).unwrap_or("probe");
    let effort = args.get(1).map(String::as_str).unwrap_or("none");
    let label = args.get(2).map(String::as_str).unwrap_or_default();
    if !matches!(scenario, "probe" | "precision" | "runtime")
        || !matches!(effort, "none" | "high")
        || args.len() > 3
        || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(
            "usage: openai_context [probe|precision|runtime] [none|high] [alphanumeric-label]"
                .into(),
        );
    }
    let model = model::OpenAiModel::discover(effort).await?;
    let policy = ContextPolicy::default().resolve(model.context_metadata())?;
    let mut report = json!({
        "requested_model":"gpt-6-luna",
        "model":model.catalog(),
        "protocol":"responses",
        "reasoning_effort":effort,
        "scenario":scenario,
        "started_at_unix_seconds":std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        "resolved_default_policy":{
            "window":policy.model_budget().context_window(),
            "input_limit":policy.model_budget().input_token_limit(),
            "threshold":policy.compaction_threshold(),
            "target":policy.compaction_target(),
            "recent_tail":policy.recent_tail_tokens(),
        },
        "limitations":[
            "Synthetic fixtures shared with the DeepSeek evaluation; not a model-quality benchmark.",
            "The runtime fixture deliberately lowers local thresholds to force repeated compaction; this does not test the provider's physical window boundary.",
            "Stateless Responses replay; no previous_response_id, provider-native compaction, streaming, or automatic transport retry.",
            "Passing functional checks does not imply accurate estimates or sufficient decision headroom.",
        ],
    });
    eprintln!(
        "catalog={} policy={}",
        report["model"], report["resolved_default_policy"]
    );
    let result = match scenario {
        "probe" => {
            let output = model
                .respond_with_metadata(ModelRequest {
                    instructions: "Reply with exactly OK.".into(),
                    messages: vec![ModelMessage::user("Connectivity probe.".into())],
                    tools: vec![],
                    max_output_tokens: Some(if effort == "none" { 64 } else { 8192 }),
                    provider_context: None,
                })
                .await;
            result_value(
                output
                    .map(|output| {
                        json!({
                            "passed":output.response.text().trim() == "OK",
                            "response_text":output.response.text(),
                            "usage":output.usage,
                        })
                    })
                    .map_err(|error| Box::new(error) as EvalError),
            )
        }
        "precision" => {
            let precision_model = model.with_output_cap(if effort == "none" { 256 } else { 8192 });
            result_value(
                precision::run(precision_model.clone(), precision_model.token_counter()).await,
            )
        }
        "runtime" => result_value(
            scenarios::run_runtime_with_counter(
                model.clone(),
                model.token_counter(),
                if effort == "none" { 1024 } else { 8192 },
                if effort == "none" { 2048 } else { 8192 },
            )
            .await,
        ),
        _ => unreachable!(),
    };
    report[scenario] = result;
    let requests = model.observations();
    report["requests"] = json!(requests);
    if effort == "high" {
        report["thinking_verified"] = json!({
            "passed":requests.iter().any(|request|
                request["usage"]["output_tokens_details"]["reasoning_tokens"]
                    .as_u64().unwrap_or(0) > 0),
            "criterion":"At least one provider response reports nonzero reasoning_tokens.",
        });
    }
    let directory = std::path::Path::new("evals/results");
    std::fs::create_dir_all(directory)?;
    let suffix = if label.is_empty() {
        String::new()
    } else {
        format!("-{label}")
    };
    let path = directory.join(format!("openai-context-{effort}-{scenario}{suffix}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    println!("report={}", path.display());
    println!(
        "{scenario}: passed={}, error={}",
        report[scenario]["passed"], report[scenario]["error"]
    );
    if report[scenario]["passed"] != true
        || (effort == "high" && report["thinking_verified"]["passed"] != true)
    {
        return Err("one or more live checks failed; inspect the saved report".into());
    }
    Ok(())
}

fn result_value(result: Result<Value, EvalError>) -> Value {
    result.unwrap_or_else(|error| json!({"passed":false,"error":error.to_string()}))
}
