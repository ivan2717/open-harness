use super::*;

pub(super) fn capture(payload: &Value) -> Result<CapturedResponse, ProviderStateError> {
    role(payload, "assistant")?;
    let mut captured = CapturedResponse::default();
    for block in array(payload, "content")? {
        let kind = identifier(block, "type")?;
        match kind.as_str() {
            "text" => captured
                .blocks
                .push(CapturedBlock::Text(string(block, "text")?.to_owned())),
            "thinking" => {
                let text = string(block, "thinking")?;
                string(block, "signature")?;
                captured
                    .blocks
                    .push(CapturedBlock::Reasoning(text.to_owned()));
            }
            "redacted_thinking" => {
                string(block, "data")?;
            }
            "tool_use" => captured.blocks.push(CapturedBlock::Call(CapturedCall {
                id: Some(identifier(block, "id")?),
                name: identifier(block, "name")?,
                arguments: json_arguments(
                    block
                        .get("input")
                        .ok_or(ProviderStateError::InvalidPayload)?,
                )?,
            })),
            kind if kind.ends_with("tool_use") || kind.ends_with("tool_result") => {
                return Err(ProviderStateError::UnsupportedToolCall);
            }
            // Preserve extension blocks, including native compaction state.
            _ => {}
        }
    }
    Ok(captured)
}
