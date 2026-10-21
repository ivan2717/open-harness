use std::collections::BTreeSet;

use agent_core::{
    model_protocol::ContentBlock,
    provider_state::{ProviderResponseState, ProviderStateError, ProviderToolCall},
};
use serde_json::{Map, Value};

use crate::WireProtocol;

mod anthropic;
mod chat;
mod gemini;
mod responses;

/// Validate a normalized response before replay. Version 1 retains ordering of
/// text and calls. Version 0 is reserved for historical flattened messages and
/// checks exact visible text plus ordered calls, as the original schema did.
/// Reasoning may be omitted from presentation; when present it is also bound.
pub fn validate_replay_state(
    state: &ProviderResponseState,
    content: &[ContentBlock],
) -> Result<(), ProviderStateError> {
    let captured = capture_state(state)?;
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::Reasoning { .. } => {}
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => calls.push(ProviderToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            ContentBlock::ToolResult { .. } => return Err(ProviderStateError::InvalidPayload),
        }
    }
    captured.validate_legacy(&text, &calls)?;
    if state.version == 0 {
        return Ok(());
    }
    let include_reasoning = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Reasoning { .. }));
    let wire = normalize(captured.blocks.iter().filter_map(|block| match block {
        CapturedBlock::Text(text) => Some(ComparableBlock::Text(text.clone())),
        CapturedBlock::Reasoning(text) if include_reasoning => {
            Some(ComparableBlock::Reasoning(text.clone()))
        }
        CapturedBlock::Reasoning(_) => None,
        CapturedBlock::Call(_) => Some(ComparableBlock::Call),
    }));
    let portable = normalize(content.iter().map(|block| match block {
        ContentBlock::Text { text } => ComparableBlock::Text(text.clone()),
        ContentBlock::Reasoning { text } => ComparableBlock::Reasoning(text.clone()),
        ContentBlock::ToolCall { .. } => ComparableBlock::Call,
        ContentBlock::ToolResult { .. } => unreachable!("tool results rejected above"),
    }));
    if wire != portable {
        return Err(ProviderStateError::ContentMismatch);
    }
    Ok(())
}

/// Compatibility helper for adapters migrating flattened historical records.
/// New production responses should use `validate_replay_state` instead.
pub fn validate_legacy_response(
    state: &ProviderResponseState,
    content: &str,
    calls: &[ProviderToolCall],
) -> Result<(), ProviderStateError> {
    capture_state(state)?.validate_legacy(content, calls)
}

fn capture_state(state: &ProviderResponseState) -> Result<CapturedResponse, ProviderStateError> {
    state.validate_envelope()?;
    if state.version > 1 {
        return Err(ProviderStateError::InvalidVersion);
    }
    capture(
        WireProtocol::try_from(state.protocol.as_str())?,
        &state.payload,
    )
}

pub(crate) fn capture(
    protocol: WireProtocol,
    payload: &Value,
) -> Result<CapturedResponse, ProviderStateError> {
    match protocol {
        WireProtocol::ChatCompletions => chat::capture(payload),
        WireProtocol::Responses => responses::capture(payload),
        WireProtocol::AnthropicMessages => anthropic::capture(payload),
        WireProtocol::GeminiGenerateContent => gemini::capture(payload),
    }
}

#[derive(Default)]
pub(crate) struct CapturedResponse {
    blocks: Vec<CapturedBlock>,
}

enum CapturedBlock {
    Text(String),
    Reasoning(String),
    Call(CapturedCall),
}

struct CapturedCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl CapturedResponse {
    fn validate_legacy(
        &self,
        content: &str,
        calls: &[ProviderToolCall],
    ) -> Result<(), ProviderStateError> {
        let text: String = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                CapturedBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if text != content {
            return Err(ProviderStateError::ContentMismatch);
        }
        let captured_calls: Vec<_> = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                CapturedBlock::Call(call) => Some(call),
                _ => None,
            })
            .collect();
        if captured_calls.len() != calls.len()
            || captured_calls.iter().zip(calls).any(|(wire, call)| {
                wire.id.as_ref().is_some_and(|id| id != &call.id)
                    || wire.name != call.name
                    || !arguments_match(&wire.arguments, &call.arguments)
            })
        {
            return Err(ProviderStateError::ToolCallsMismatch);
        }
        Ok(())
    }

    pub(crate) fn into_content(self, response_namespace: &str) -> Vec<ContentBlock> {
        let mut used_ids: BTreeSet<_> = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                CapturedBlock::Call(call) => call.id.clone(),
                _ => None,
            })
            .collect();
        let mut generated_id = 0usize;
        self.blocks
            .into_iter()
            .map(|block| match block {
                CapturedBlock::Text(text) => ContentBlock::Text { text },
                CapturedBlock::Reasoning(text) => ContentBlock::Reasoning { text },
                CapturedBlock::Call(call) => {
                    let id = call.id.unwrap_or_else(|| {
                        loop {
                            generated_id += 1;
                            let candidate = format!("{response_namespace}:call:{generated_id}");
                            if used_ids.insert(candidate.clone()) {
                                break candidate;
                            }
                        }
                    });
                    ContentBlock::ToolCall {
                        id,
                        name: call.name,
                        arguments: call.arguments,
                    }
                }
            })
            .collect()
    }
}

#[derive(PartialEq, Eq)]
enum ComparableBlock {
    Text(String),
    Reasoning(String),
    Call,
}

// Streaming chunk and adjacent text-part boundaries carry no portable semantic
// difference. Do preserve every transition around calls or displayed reasoning.
fn normalize(blocks: impl Iterator<Item = ComparableBlock>) -> Vec<ComparableBlock> {
    let mut result = Vec::new();
    for block in blocks {
        match block {
            ComparableBlock::Text(text) if text.is_empty() => {}
            ComparableBlock::Reasoning(text) if text.is_empty() => {}
            ComparableBlock::Text(text) => match result.last_mut() {
                Some(ComparableBlock::Text(previous)) => previous.push_str(&text),
                _ => result.push(ComparableBlock::Text(text)),
            },
            ComparableBlock::Reasoning(text) => match result.last_mut() {
                Some(ComparableBlock::Reasoning(previous)) => previous.push_str(&text),
                _ => result.push(ComparableBlock::Reasoning(text)),
            },
            ComparableBlock::Call => result.push(ComparableBlock::Call),
        }
    }
    result
}

fn object(value: &Value) -> Result<&Map<String, Value>, ProviderStateError> {
    value.as_object().ok_or(ProviderStateError::InvalidPayload)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderStateError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProviderStateError::InvalidPayload)
}

fn identifier(value: &Value, key: &str) -> Result<String, ProviderStateError> {
    let result = string(value, key)?;
    if result.trim().is_empty() {
        return Err(ProviderStateError::InvalidPayload);
    }
    Ok(result.to_owned())
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], ProviderStateError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(ProviderStateError::InvalidPayload)
}

fn role(value: &Value, expected: &str) -> Result<(), ProviderStateError> {
    object(value)?;
    if string(value, "role")? != expected {
        return Err(ProviderStateError::InvalidPayload);
    }
    Ok(())
}

fn optional_string(value: &Value, key: &str) -> Result<(), ProviderStateError> {
    if value
        .get(key)
        .is_some_and(|field| !field.is_null() && !field.is_string())
    {
        return Err(ProviderStateError::InvalidPayload);
    }
    Ok(())
}

fn arguments_match(wire: &str, portable: &str) -> bool {
    wire == portable
        || matches!(
            (serde_json::from_str::<Value>(wire), serde_json::from_str::<Value>(portable)),
            (Ok(left), Ok(right)) if left == right
        )
}

fn json_arguments(value: &Value) -> Result<String, ProviderStateError> {
    object(value)?;
    serde_json::to_string(value).map_err(|_| ProviderStateError::InvalidPayload)
}

#[cfg(test)]
mod tests;
