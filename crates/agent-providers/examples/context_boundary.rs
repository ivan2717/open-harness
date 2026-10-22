//! Opt-in, bounded physical context-boundary probes through the production adapter.
//!
//! cargo run -p agent-providers --example context_boundary --
//!   deepseek 1000000 evals/results/deepseek-boundary.json [all|calibration|near90|near98|over102]
//!
//! Optional near88 / near85 / near75 / near50 / near25 stages investigate a lower
//! deployed capacity. They require an existing successful calibration and are
//! never included in `all`. Each report remains bounded to five invocations.
//!
//! Supply DEEPSEEK_API_KEY / OPENAI_API_KEY in the environment. Optional MODEL_ID
//! and API_BASE_URL select a deployed model; context capacity must be verified
//! independently. No tokenizer or separate token-count API is used. These direct
//! probes deliberately bypass runtime compaction to measure the physical limit.

use agent_providers::{HttpModel, ProviderConfig, ProviderProtocol, ReasoningMode};
use agent_runtime::{
    ApproximateTokenCounter, ContextMeasurement, ContextPolicy, Model, TokenCounter,
};
use serde_json::{Value, json};
use std::{error::Error, fs, path::Path, time::Instant};

#[path = "boundary_support/diagnostic.rs"]
mod diagnostic;
#[path = "boundary_support/fixture.rs"]
mod fixture;
use fixture::{CALIBRATION_ROWS, FIXTURE_VERSION, OUTPUT_RESERVE, fixture_request, scaled_rows};

const MAX_MODEL_INVOCATIONS: usize = 5;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err("usage: context_boundary deepseek|openai WINDOW REPORT_PATH [all|calibration|near25|near50|near75|near85|near88|near90|near98|over102|diagnose90|diagnose102]".into());
    }
    let provider = args[0].as_str();
    let window: usize = args[1].parse()?;
    if !(128_000..=2_000_000).contains(&window) {
        return Err("this bounded evaluator accepts windows from 128,000 to 2,000,000".into());
    }
    let report_path = Path::new(&args[2]);
    let stage = args.get(3).map(String::as_str).unwrap_or("all");
    let stages: &[&str] = match stage {
        "all" => &["calibration", "near90", "near98", "over102"],
        "calibration" => &["calibration"],
        "near25" => &["near25"],
        "near50" => &["near50"],
        "near75" => &["near75"],
        "near85" => &["near85"],
        "near88" => &["near88"],
        "near90" => &["near90"],
        "near98" => &["near98"],
        "over102" => &["over102"],
        "diagnose90" | "diagnose102" => &[],
        _ => return Err("unknown stage: expected all, calibration, near25, near50, near75, near85, near88, near90, near98, over102, diagnose90 or diagnose102".into()),
    };
    let (protocol, key_env, default_model, default_base, reasoning) = match provider {
        "deepseek" => (
            ProviderProtocol::ChatCompletions,
            "DEEPSEEK_API_KEY",
            "deepseek-flash",
            "https://api.deepseek.com",
            ReasoningMode::DeepSeek {
                effort: "high".into(),
            },
        ),
        "openai" => (
            ProviderProtocol::Responses,
            "OPENAI_API_KEY",
            "gpt-6-luna",
            "https://api.openai.com/v1",
            ReasoningMode::Effort {
                effort: "high".into(),
            },
        ),
        _ => return Err("provider must be deepseek or openai".into()),
    };
    let model_id = std::env::var("MODEL_ID").unwrap_or_else(|_| default_model.into());
    let model = HttpModel::new(ProviderConfig {
        protocol,
        base_url: std::env::var("API_BASE_URL").unwrap_or_else(|_| default_base.into()),
        api_key: std::env::var(key_env)
            .map_err(|_| "missing provider API key environment variable")?,
        model: model_id.clone(),
        context_window: window,
        max_output_tokens: OUTPUT_RESERVE,
        reasoning,
    })?;
    let policy = ContextPolicy::builder()
        .output_token_reserve(OUTPUT_RESERVE)
        .build()?
        .resolve(model.context_metadata())?;
    let mut report = if report_path.exists() {
        let existing: Value = serde_json::from_slice(&fs::read(report_path)?)?;
        if existing["provider"] != provider
            || existing["model"] != model_id
            || existing["context_window"] != window
            || existing["fixture_version"] != FIXTURE_VERSION
            || !existing["stages"].is_array()
        {
            return Err("existing report identity does not match this invocation".into());
        }
        existing
    } else {
        json!({
            "provider": provider,
            "model": model_id,
            "protocol": protocol,
            "context_window": window,
            "output_reserve": OUTPUT_RESERVE,
            "reasoning": "high",
            "fixture_version": FIXTURE_VERSION,
            "transport": "agent_providers::HttpModel",
            "scope": "direct provider physical boundary; bypasses runtime context management",
            "limits": {
                "max_model_invocations": MAX_MODEL_INVOCATIONS,
                "request_timeout_seconds": 600,
                "transport_retry_note": "HttpModel may retry connection establishment or explicit 429/5xx before streaming, at most twice; no retry after output starts",
                "no_tokenizer_or_token_count_endpoint": true
            },
            "default_runtime_policy_for_comparison": {
                "input_limit": policy.input_token_limit(),
                "compaction_threshold": policy.compaction_threshold(),
                "compaction_target": policy.compaction_target(),
                "estimation_margin_percent": policy.estimation_margin_percent()
            },
            "stages": []
        })
    };

    if matches!(stage, "diagnose90" | "diagnose102") {
        let prior_stage = if stage == "diagnose90" {
            "near90"
        } else {
            "over102"
        };
        let prior = report["stages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry["stage"] == prior_stage)
            .ok_or("diagnostic requires its original boundary stage")?;
        let rows = prior["rows"].as_u64().ok_or("missing original row count")? as usize;
        let previous_diagnostics = report
            .get("diagnostics")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if previous_diagnostics > 0
            || report["stages"].as_array().unwrap().len() + previous_diagnostics
                >= MAX_MODEL_INVOCATIONS
        {
            return Err("one bounded diagnostic per report is allowed".into());
        }
        report["diagnostics"] = json!([{"stage": stage, "repeats_stage": prior_stage, "rows": rows, "status": "running"}]);
        save_report(report_path, &report)?;
        let result = diagnostic::run(provider, &model_id, rows, key_env).await;
        let record = &mut report["diagnostics"][0];
        match result {
            Ok(details) => {
                record["status"] = json!("completed");
                record["details"] = details;
            }
            Err(error) => {
                record["status"] = json!("error");
                record["error"] = json!(error.to_string());
            }
        }
        save_report(report_path, &report)?;
        println!("{provider} {stage}: {}", report["diagnostics"][0]);
        return Ok(());
    }

    for stage in stages {
        if report["stages"].as_array().unwrap().len()
            + report
                .get("diagnostics")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
            >= MAX_MODEL_INVOCATIONS
        {
            return Err("bounded evaluation invocation limit reached".into());
        }
        let target = match *stage {
            "calibration" => None,
            "near25" => Some(window * 25 / 100),
            "near50" => Some(window * 50 / 100),
            "near75" => Some(window * 75 / 100),
            "near85" => Some(window * 85 / 100),
            "near88" => Some(window * 88 / 100),
            "near90" => Some(window * 90 / 100),
            "near98" => Some(window * 98 / 100),
            "over102" => Some(window * 102 / 100),
            _ => unreachable!(),
        };
        let (rows, calibration_basis) = if let Some(target) = target {
            // Use the largest successful preceding stage. This keeps the final
            // boundary extrapolation short, without adding a binary-search loop.
            let anchor = report["stages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| {
                    item["status"] == "completed"
                        && item["usage"]["input_tokens"].as_u64().is_some()
                })
                .max_by_key(|item| item["usage"]["input_tokens"].as_u64().unwrap());
            let anchor = anchor.ok_or("a completed calibration with provider usage is required")?;
            let anchor_tokens = anchor["usage"]["input_tokens"].as_u64().unwrap() as usize;
            let anchor_rows = anchor["rows"].as_u64().ok_or("missing calibration rows")? as usize;
            if anchor_tokens < 1000 {
                return Err("calibration usage unexpectedly small".into());
            }
            (
                scaled_rows(anchor_rows, anchor_tokens, target),
                json!({
                    "stage": anchor["stage"],
                    "rows": anchor_rows,
                    "actual_input_tokens": anchor_tokens,
                    "method": "row count scaled from real generation usage; target size is not a measured count"
                }),
            )
        } else {
            (CALIBRATION_ROWS, Value::Null)
        };
        if rows > 300_000 {
            return Err("fixture exceeds the hard 300,000-row bound".into());
        }
        let preparation_start = Instant::now();
        let request = fixture_request(rows);
        let request_text_bytes = request.instructions.len() + request.messages[0].text().len();
        let canonical_json_bytes = serde_json::to_vec(&request)?.len();
        let counter = ApproximateTokenCounter::default();
        let measurement = ContextMeasurement::estimated(counter.count_tokens(&request))
            .with_margin_percent(policy.estimation_margin_percent());
        let preparation_ms = preparation_start.elapsed().as_millis();
        let entry = json!({
            "stage": stage,
            "status": "running",
            "target_input_tokens": target,
            "rows": rows,
            "calibration_basis": calibration_basis,
            "request_text_bytes": request_text_bytes,
            "canonical_json_bytes": canonical_json_bytes,
            "local_measurement": measurement,
            "preparation_ms": preparation_ms,
            "cold_runtime_would_cross_hard_input_limit": measurement.decision_input_tokens > policy.input_token_limit(),
            "note": "The runtime protects the latest user message; this whole-fixture user message is deliberately sent directly."
        });
        report["stages"].as_array_mut().unwrap().push(entry);
        save_report(report_path, &report)?;
        println!(
            "{provider} {stage}: starting {rows} rows; raw={} decision={} target={target:?}",
            measurement.raw_estimated_input_tokens, measurement.decision_input_tokens
        );
        let started = Instant::now();
        let result = model.respond_with_metadata(request).await;
        let elapsed_ms = started.elapsed().as_millis();
        let entry = report["stages"].as_array_mut().unwrap().last_mut().unwrap();
        entry["elapsed_ms"] = json!(elapsed_ms);
        match result {
            Ok(output) => {
                let answer = output.response.text();
                let parsed = parse_answer(&answer);
                let needles_ok = parsed.as_ref().is_some_and(|answer| {
                    answer["start"] == "MINT-R4"
                        && answer["middle"] == "COBALT-P8"
                        && answer["end"] == "LARCH-V2"
                });
                entry["status"] = json!("completed");
                entry["usage"] = json!(output.usage);
                entry["finish_reason"] = json!(output.response.finish_reason);
                entry["answer"] = json!(answer.chars().take(2048).collect::<String>());
                entry["needles_correct"] = json!(needles_ok);
                entry["has_provider_replay_state"] = json!(output.provider_state.is_some());
                entry["reasoning_blocks"] = json!(
                    output
                        .response
                        .content
                        .iter()
                        .filter(|block| matches!(
                            block,
                            agent_runtime::ContentBlock::Reasoning { .. }
                        ))
                        .count()
                );
                if let Some(usage) = output.usage.filter(|usage| usage.input_tokens > 0) {
                    let actual = usage.input_tokens;
                    entry["actual_input_fraction_of_window"] = json!(actual as f64 / window as f64);
                    entry["raw_estimation_error_percent"] = json!(
                        (measurement.raw_estimated_input_tokens as f64 / actual as f64 - 1.0)
                            * 100.0
                    );
                    entry["decision_error_percent"] = json!(
                        (measurement.decision_input_tokens as f64 / actual as f64 - 1.0) * 100.0
                    );
                    entry["actual_plus_reserved_output"] = json!(actual + OUTPUT_RESERVE);
                }
                println!(
                    "{provider} {stage}: completed in {elapsed_ms} ms; usage={:?}; needles={needles_ok}",
                    output.usage
                );
            }
            Err(error) => {
                entry["status"] = json!("error");
                entry["error"] = json!(error.to_string());
                entry["error_class"] = json!(format!("{error:?}"));
                entry["context_overflow"] = json!(model.is_context_overflow(&error));
                println!("{provider} {stage}: failed in {elapsed_ms} ms: {error}");
            }
        }
        save_report(report_path, &report)?;
        if *stage == "calibration" && entry_status(&report) != Some("completed") {
            return Err("calibration failed; no large requests were attempted".into());
        }
    }
    Ok(())
}

fn entry_status(report: &Value) -> Option<&str> {
    report["stages"].as_array()?.last()?["status"].as_str()
}

fn save_report(path: &Path, report: &Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(report)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn parse_answer(answer: &str) -> Option<Value> {
    let answer = answer.trim();
    serde_json::from_str(answer).ok().or_else(|| {
        let start = answer.find('{')?;
        let end = answer.rfind('}')?;
        serde_json::from_str(&answer[start..=end]).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_fixture_has_exactly_one_of_each_needle_at_all_target_sizes() {
        for rows in [CALIBRATION_ROWS, 100_000, 130_000] {
            let request = fixture_request(rows);
            let text = request.messages[0].text();
            for marker in ["NEEDLE_START =", "NEEDLE_MIDDLE =", "NEEDLE_END ="] {
                assert_eq!(text.matches(marker).count(), 1);
            }
            assert!(text.len() <= rows * 36 + 512);
            assert!(
                request
                    .validate_provider_state_with(None, false, |_, _| Ok(()))
                    .is_ok()
            );
        }
    }

    #[test]
    fn real_usage_scaling_does_not_use_local_estimate() {
        assert_eq!(scaled_rows(4096, 32_768, 980_000), 122_500);
        assert_eq!(scaled_rows(122_500, 980_000, 1_020_000), 127_500);
    }
}
