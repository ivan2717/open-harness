use agent_runtime::{Model, ModelFinishReason, ModelMessage, ModelRequest, TokenCounter};
use serde_json::{Value, json};

use super::model::DeepSeekModel;

/// Compares input-only prefix calibration with the former input-plus-output
/// formula on a real no-tools request. DeepSeek ignores prior reasoning here,
/// even though its exact assistant state is durably retained and replayed.
pub async fn run(model: DeepSeekModel) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let first = ModelRequest {
        instructions: "Solve the requested problem accurately. Return only the requested final result.".into(),
        messages: vec![ModelMessage::user("Solve SEND + MORE = MONEY in base 10, where distinct letters are distinct digits and S and M are nonzero. Return only the completed numeric equation.".into())],
        tools: vec![],
        max_output_tokens: Some(8192),
        provider_context: None,
    };
    let first_output = model.respond_with_metadata(first.clone()).await?;
    let first_usage = first_output.usage.ok_or("missing first usage")?;
    if first_output.response.finish_reason != ModelFinishReason::Stop {
        return Err("accounting probe expected a final answer".into());
    }
    let answer = first_output.response.text();
    let counter = model.token_counter();
    let original_input_estimate = counter.count_tokens(&first);
    let mut prefix = first;
    prefix.messages.push(ModelMessage::assistant_blocks(
        first_output.response.content,
        first_output.provider_state,
    ));
    let mut next = prefix.clone();
    next.messages.push(ModelMessage::user(
        "Repeat exactly that numeric equation, without explanation.".into(),
    ));
    let new_message_tokens = counter.count_tokens(&next) - counter.count_tokens(&prefix);
    let replayed_extension = counter.count_tokens(&next) - original_input_estimate;
    let former_prediction =
        first_usage.input_tokens + first_usage.output_tokens + new_message_tokens;
    let calibrated_prediction = first_usage.input_tokens + replayed_extension;
    let second_output = model.respond_with_metadata(next).await?;
    let second_usage = second_output.usage.ok_or("missing second usage")?;
    let observations = model.observations();
    let first_observation = &observations[observations.len() - 2];
    let reasoning_tokens =
        first_observation["usage"]["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0);
    let former_discrepancy = former_prediction as i64 - second_usage.input_tokens as i64;
    let calibrated_discrepancy = calibrated_prediction as i64 - second_usage.input_tokens as i64;
    Ok(json!({
        "passed": reasoning_tokens > 0
            && calibrated_discrepancy.unsigned_abs() < former_discrepancy.unsigned_abs(),
        "scope":"Compare input-only prefix calibration with the former formula; the retained response and new user message are estimated as an extension, while DeepSeek excludes old reasoning without tools.",
        "previous_input_tokens":first_usage.input_tokens,
        "previous_output_tokens_including_reasoning":first_usage.output_tokens,
        "previous_reasoning_tokens":reasoning_tokens,
        "estimated_new_message_tokens":new_message_tokens,
        "estimated_replayed_response_and_new_message_tokens":replayed_extension,
        "former_input_plus_output_predicted_input_tokens":former_prediction,
        "input_only_calibrated_prediction":calibrated_prediction,
        "actual_next_input_tokens":second_usage.input_tokens,
        "former_prediction_minus_actual":former_discrepancy,
        "calibrated_prediction_minus_actual":calibrated_discrepancy,
        "former_reasoning_overcount_observed":former_discrepancy > 0
            && former_discrepancy as u64 > reasoning_tokens / 2,
        "visible_answer":answer,
    }))
}
