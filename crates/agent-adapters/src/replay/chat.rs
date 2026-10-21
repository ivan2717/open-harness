use super::*;

pub(super) fn capture(payload: &Value) -> Result<CapturedResponse, ProviderStateError> {
    role(payload, "assistant")?;
    if payload
        .get("function_call")
        .is_some_and(|value| !value.is_null())
    {
        return Err(ProviderStateError::UnsupportedToolCall);
    }
    optional_string(payload, "refusal")?;
    let mut captured = CapturedResponse::default();
    // OpenAI-compatible providers use distinct reasoning field names. Keep the
    // exact field in payload; the portable block is only its readable content.
    for key in ["reasoning_content", "reasoning", "reasoning_text"] {
        optional_string(payload, key)?;
        if let Some(Value::String(text)) = payload.get(key)
            && !text.is_empty()
        {
            captured.blocks.push(CapturedBlock::Reasoning(text.clone()));
            break;
        }
    }
    match payload.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => captured.blocks.push(CapturedBlock::Text(text.clone())),
        Some(Value::Array(parts)) => {
            for part in parts {
                let text = match string(part, "type")? {
                    "text" => string(part, "text")?,
                    "refusal" => string(part, "refusal")?,
                    _ => return Err(ProviderStateError::InvalidPayload),
                };
                captured.blocks.push(CapturedBlock::Text(text.to_owned()));
            }
        }
        _ => return Err(ProviderStateError::InvalidPayload),
    }
    if let Some(Value::String(refusal)) = payload.get("refusal") {
        captured.blocks.push(CapturedBlock::Text(refusal.clone()));
    }
    match payload.get("tool_calls") {
        None | Some(Value::Null) => {}
        Some(Value::Array(calls)) => {
            for call in calls {
                if string(call, "type")? != "function" {
                    return Err(ProviderStateError::UnsupportedToolCall);
                }
                let function = call
                    .get("function")
                    .ok_or(ProviderStateError::InvalidPayload)?;
                captured.blocks.push(CapturedBlock::Call(CapturedCall {
                    id: Some(identifier(call, "id")?),
                    name: identifier(function, "name")?,
                    arguments: string(function, "arguments")?.to_owned(),
                }));
            }
        }
        _ => return Err(ProviderStateError::InvalidPayload),
    }
    Ok(captured)
}
