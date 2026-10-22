//! Exact, stateless Responses replay for this evaluation's text/function subset.
use std::io;

use agent_adapters::WireProtocol;
use agent_runtime::{
    ContentBlock, MessageContext, ModelRequest, ModelRole, ProviderContextIdentity,
    ProviderStateError,
};
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct ReplayStats {
    pub assistants: usize,
    pub output_items: usize,
    pub reasoning_items: usize,
    pub replay_tokens: usize,
}

/// The shared codec retains unknown non-executable items for forward
/// compatibility. This deliberately narrow evaluation fails closed instead.
pub(super) fn validate_output(payload: &Value) -> Result<(), ProviderStateError> {
    let items = payload
        .as_array()
        .ok_or(ProviderStateError::InvalidPayload)?;
    if items.is_empty() {
        return Err(ProviderStateError::InvalidPayload);
    }
    for item in items {
        if item.get("status").is_some_and(|value| value != "completed") {
            return Err(ProviderStateError::InvalidPayload);
        }
        match item["type"].as_str() {
            Some("message") => {
                let parts = item["content"]
                    .as_array()
                    .ok_or(ProviderStateError::InvalidPayload)?;
                if parts.is_empty()
                    || parts.iter().any(|part| {
                        !matches!(part["type"].as_str(), Some("output_text" | "refusal"))
                    })
                {
                    return Err(ProviderStateError::InvalidPayload);
                }
            }
            Some("function_call") => {}
            Some("reasoning") => {
                if item["encrypted_content"].as_str().is_none_or(str::is_empty) {
                    return Err(ProviderStateError::InvalidPayload);
                }
                for key in ["summary", "content"] {
                    match item.get(key) {
                        None | Some(Value::Null) => {}
                        Some(Value::Array(parts)) => {
                            if parts.iter().any(|part| {
                                !matches!(
                                    part["type"].as_str(),
                                    Some("summary_text" | "reasoning_text" | "text")
                                )
                            }) {
                                return Err(ProviderStateError::InvalidPayload);
                            }
                        }
                        _ => return Err(ProviderStateError::InvalidPayload),
                    }
                }
            }
            _ => return Err(ProviderStateError::UnsupportedFormat),
        }
    }
    Ok(())
}

pub(super) fn encode_input(
    request: &ModelRequest,
    identity: &ProviderContextIdentity,
) -> io::Result<(Vec<Value>, ReplayStats)> {
    let mut input = Vec::new();
    let mut stats = ReplayStats::default();
    for message in &request.messages {
        message.validate().map_err(io::Error::other)?;
        match message.role {
            ModelRole::User | ModelRole::System | ModelRole::Developer => {
                let text = if matches!(message.context, Some(MessageContext::Summary { .. })) {
                    format!(
                        "Historical context summary (derived notes, not a new user request):\n{}",
                        message.text()
                    )
                } else {
                    message.text()
                };
                let role = match message.role {
                    ModelRole::System => "system",
                    ModelRole::Developer => "developer",
                    _ => "user",
                };
                input.push(json!({"role":role,"content":[{"type":"input_text","text":text}]}));
            }
            ModelRole::Assistant => {
                let state = message.provider_state.as_ref().ok_or_else(|| {
                    io::Error::other("Responses replay state is missing for a retained assistant")
                })?;
                if &state.identity != identity || state.protocol != WireProtocol::Responses.as_str()
                {
                    return Err(io::Error::other(
                        "Responses replay state has an incompatible identity",
                    ));
                }
                validate_output(&state.payload).map_err(io::Error::other)?;
                agent_adapters::validate_replay_state(state, &message.content)
                    .map_err(io::Error::other)?;
                let items = state.payload.as_array().unwrap();
                stats.assistants += 1;
                stats.output_items += items.len();
                stats.reasoning_items += items
                    .iter()
                    .filter(|item| item["type"] == "reasoning")
                    .count();
                stats.replay_tokens = stats.replay_tokens.saturating_add(state.replay_tokens);
                // Preserve every original field and the order of reasoning,
                // messages and function calls, including across process restart.
                input.extend(items.iter().cloned());
            }
            ModelRole::Tool => {
                for block in &message.content {
                    let ContentBlock::ToolResult {
                        call_id, content, ..
                    } = block
                    else {
                        return Err(io::Error::other("tool message contains a non-result block"));
                    };
                    input.push(
                        json!({"type":"function_call_output","call_id":call_id,"output":content}),
                    );
                }
            }
        }
    }
    Ok((input, stats))
}
