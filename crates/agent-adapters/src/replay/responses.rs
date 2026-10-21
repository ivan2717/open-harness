use super::*;

pub(super) fn capture(payload: &Value) -> Result<CapturedResponse, ProviderStateError> {
    let items = payload
        .as_array()
        .ok_or(ProviderStateError::InvalidPayload)?;
    let mut captured = CapturedResponse::default();
    for item in items {
        object(item)?;
        if item.get("role").is_some_and(|value| value != "assistant") {
            return Err(ProviderStateError::InvalidPayload);
        }
        let kind = identifier(item, "type")?;
        match kind.as_str() {
            "message" => {
                role(item, "assistant")?;
                for part in array(item, "content")? {
                    let text = match string(part, "type")? {
                        "output_text" => string(part, "text")?,
                        "refusal" => string(part, "refusal")?,
                        _ => return Err(ProviderStateError::InvalidPayload),
                    };
                    captured.blocks.push(CapturedBlock::Text(text.to_owned()));
                }
            }
            "function_call" => captured.blocks.push(CapturedBlock::Call(CapturedCall {
                id: Some(identifier(item, "call_id")?),
                name: identifier(item, "name")?,
                arguments: string(item, "arguments")?.to_owned(),
            })),
            "reasoning" => {
                optional_string(item, "encrypted_content")?;
                // OpenAI summaries and DeepSeek plaintext are different wire
                // fields. Display their readable text without rewriting either.
                for key in ["summary", "content"] {
                    match item.get(key) {
                        None | Some(Value::Null) => {}
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                object(part)?;
                                match string(part, "type")? {
                                    "summary_text" | "reasoning_text" | "text" => {
                                        captured.blocks.push(CapturedBlock::Reasoning(
                                            string(part, "text")?.to_owned(),
                                        ));
                                    }
                                    _ => {} // Opaque extension retained in raw state.
                                }
                            }
                        }
                        _ => return Err(ProviderStateError::InvalidPayload),
                    }
                }
            }
            "compaction" => optional_string(item, "encrypted_content")?,
            kind if kind.ends_with("_call") || kind.ends_with("_call_output") => {
                return Err(ProviderStateError::UnsupportedToolCall);
            }
            // Unknown non-executable items are retained without interpretation.
            _ => {}
        }
    }
    Ok(captured)
}
