//! Stateless projection of canonical content blocks onto DeepSeek's wire format.
//! Original provider state is validated by the adapter before exact replay.
use std::io;

use agent_adapters::{WireProtocol, validate_replay_state};
use agent_runtime::{
    ContentBlock, MessageContext, ModelRequest, ModelRole, ProviderContextIdentity,
};
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct ReplayStats {
    pub assistants: usize,
    pub reasoning_chars: usize,
    pub reasoning_contains_archive_pin: bool,
}

pub(super) fn encode_messages(
    request: &ModelRequest,
    identity: &ProviderContextIdentity,
    replay_reasoning: bool,
) -> io::Result<(Vec<Value>, ReplayStats)> {
    let mut messages = vec![json!({"role":"system","content":request.instructions})];
    let mut stats = ReplayStats::default();
    for message in &request.messages {
        match message.role {
            ModelRole::User | ModelRole::System | ModelRole::Developer => {
                let content = if matches!(message.context, Some(MessageContext::Summary { .. })) {
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
                messages.push(json!({"role":role,"content":content}));
            }
            ModelRole::Assistant => {
                let calls = message.tool_calls();
                let original = if replay_reasoning {
                    match &message.provider_state {
                        Some(state) => {
                            if &state.identity != identity
                                || state.protocol != WireProtocol::ChatCompletions.as_str()
                            {
                                return Err(io::Error::other(
                                    "durable response state does not match the DeepSeek adapter",
                                ));
                            }
                            validate_replay_state(state, &message.content)
                                .map_err(io::Error::other)?;
                            if !calls.is_empty()
                                && state.payload["reasoning_content"].as_str().is_none()
                            {
                                return Err(io::Error::other(
                                    "durable thinking tool-call response is missing reasoning_content",
                                ));
                            }
                            stats.assistants += 1;
                            let reasoning = state.payload["reasoning_content"]
                                .as_str()
                                .unwrap_or_default();
                            stats.reasoning_chars += reasoning.chars().count();
                            stats.reasoning_contains_archive_pin |= reasoning.contains("CEDAR-7Q9");
                            Some(state.payload.clone())
                        }
                        None if !calls.is_empty() => {
                            return Err(io::Error::other(
                                "thinking replay state is missing for retained assistant tool calls",
                            ));
                        }
                        None => None,
                    }
                } else {
                    None
                };
                messages.push(original.unwrap_or_else(|| {
                    let mut wire = json!({"role":"assistant","content":message.text()});
                    if !calls.is_empty() {
                        wire["tool_calls"] = Value::Array(calls.iter().map(|call| json!({
                            "id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}
                        })).collect());
                    }
                    wire
                }));
            }
            ModelRole::Tool => {
                for block in &message.content {
                    let ContentBlock::ToolResult {
                        call_id, content, ..
                    } = block
                    else {
                        return Err(io::Error::other("tool message contains a non-result block"));
                    };
                    messages.push(json!({"role":"tool","tool_call_id":call_id,"content":content}));
                }
            }
        }
    }
    Ok((messages, stats))
}
