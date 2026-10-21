use super::*;

pub(super) fn capture(payload: &Value) -> Result<CapturedResponse, ProviderStateError> {
    role(payload, "model")?;
    let mut captured = CapturedResponse::default();
    for part in array(payload, "parts")? {
        object(part)?;
        optional_string(part, "thoughtSignature")?;
        if part.get("thought").is_some_and(|value| !value.is_boolean()) {
            return Err(ProviderStateError::InvalidPayload);
        }
        if let Some(text) = part.get("text") {
            let text = text
                .as_str()
                .ok_or(ProviderStateError::InvalidPayload)?
                .to_owned();
            captured
                .blocks
                .push(if part.get("thought") == Some(&Value::Bool(true)) {
                    CapturedBlock::Reasoning(text)
                } else {
                    CapturedBlock::Text(text)
                });
        }
        if let Some(call) = part.get("functionCall") {
            object(call)?;
            let id = call.get("id").map(|_| identifier(call, "id")).transpose()?;
            let arguments = match call.get("args") {
                Some(args) => json_arguments(args)?,
                None => "{}".to_owned(),
            };
            captured.blocks.push(CapturedBlock::Call(CapturedCall {
                id,
                name: identifier(call, "name")?,
                arguments,
            }));
        }
        if ["functionResponse", "executableCode", "codeExecutionResult"]
            .iter()
            .any(|key| part.get(*key).is_some())
        {
            return Err(ProviderStateError::UnsupportedToolCall);
        }
    }
    Ok(captured)
}
