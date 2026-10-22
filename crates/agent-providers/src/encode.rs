use super::*;
use agent_runtime::{MessageContext, ModelMessage, ModelRole};
use serde_json::{Value, json};

impl HttpModel {
    pub(super) fn encode(&self, request: &ModelRequest) -> Result<Value, ProviderError> {
        self.validate_request(request)
            .map_err(|_| ProviderError::InvalidRequest)?;
        if request.provider_context.is_some() {
            return Err(ProviderError::InvalidRequest);
        }
        for message in &request.messages {
            message
                .validate()
                .map_err(|_| ProviderError::InvalidRequest)?;
        }
        let max = request
            .max_output_tokens
            .unwrap_or(self.metadata.max_output_tokens.unwrap())
            .min(self.metadata.max_output_tokens.unwrap());
        if max == 0 {
            return Err(ProviderError::InvalidRequest);
        }
        let mut body = match self.protocol {
            ProviderProtocol::ChatCompletions => {
                let mut messages = vec![];
                if !request.instructions.is_empty() {
                    messages.push(json!({"role":"system","content":request.instructions}));
                }
                for message in &request.messages {
                    messages.extend(chat(message)?);
                }
                let mut body = json!({"model":self.model,"messages":messages,"stream":true,"stream_options":{"include_usage":true}});
                let field = if matches!(self.reasoning, ReasoningMode::Effort { .. }) {
                    "max_completion_tokens"
                } else {
                    "max_tokens"
                };
                body[field] = json!(max);
                body
            }
            ProviderProtocol::Responses => {
                let mut input = vec![];
                for message in &request.messages {
                    input.extend(responses(message)?);
                }
                json!({"model":self.model,"instructions":request.instructions,"input":input,"stream":true,
                    "store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":max})
            }
            ProviderProtocol::AnthropicMessages => {
                let mut messages = vec![];
                let mut system = vec![];
                if !request.instructions.is_empty() {
                    system.push(json!({"type":"text","text":request.instructions}));
                }
                for message in &request.messages {
                    if matches!(message.role, ModelRole::System | ModelRole::Developer) {
                        system.push(json!({"type":"text","text":message_text(message)}));
                    } else {
                        messages.push(anthropic(message)?);
                    }
                }
                json!({"model":self.model,"system":system,"messages":messages,"stream":true,"max_tokens":max})
            }
        };
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(|tool| {
                let schema: Value = serde_json::from_str(&tool.input_schema).map_err(|_| ProviderError::InvalidRequest)?;
                if !schema.is_object() { return Err(ProviderError::InvalidRequest); }
                Ok(match self.protocol {
                    ProviderProtocol::ChatCompletions => json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":schema}}),
                    ProviderProtocol::Responses => json!({"type":"function","name":tool.name,"description":tool.description,"parameters":schema,"strict":false}),
                    ProviderProtocol::AnthropicMessages => json!({"name":tool.name,"description":tool.description,"input_schema":schema}),
                })
            }).collect::<Result<_, _>>()?);
        }
        match &self.reasoning {
            ReasoningMode::Off => {}
            ReasoningMode::Effort { effort } => {
                if self.protocol == ProviderProtocol::Responses {
                    body["reasoning"] = json!({"effort":effort});
                } else {
                    body["reasoning_effort"] = json!(effort);
                }
            }
            ReasoningMode::DeepSeek { effort } => {
                body["thinking"] = json!({"type":"enabled"});
                body["reasoning_effort"] = json!(effort);
            }
            ReasoningMode::AnthropicBudget { tokens } => {
                // Compactor requests can have a lower output cap than normal turns.
                // Explicit thinking needs >=1024 tokens strictly below max_tokens.
                if max > 1024 {
                    body["thinking"] =
                        json!({"type":"enabled","budget_tokens":(*tokens).min(max - 1)});
                }
            }
        }
        Ok(body)
    }
}
fn role(role: ModelRole) -> &'static str {
    match role {
        ModelRole::System => "system",
        ModelRole::Developer => "developer",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    }
}
fn message_text(message: &ModelMessage) -> String {
    if matches!(message.context, Some(MessageContext::Summary { .. })) {
        format!(
            "Historical context summary (derived notes, not a new user request):\n{}",
            message.text()
        )
    } else {
        message.text()
    }
}
fn chat(message: &ModelMessage) -> Result<Vec<Value>, ProviderError> {
    if let Some(state) = &message.provider_state {
        return Ok(vec![state.payload.clone()]);
    }
    if message.role == ModelRole::Tool {
        return Ok(message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    call_id, content, ..
                } => Some(json!({"role":"tool","tool_call_id":call_id,"content":content})),
                _ => None,
            })
            .collect());
    }
    if message
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Reasoning { .. }))
    {
        return Err(ProviderError::InvalidRequest);
    }
    let mut value = json!({"role":role(message.role),"content":message_text(message)});
    let calls = message.tool_calls();
    if !calls.is_empty() {
        value["tool_calls"] = json!(calls.iter().map(|call| json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}})).collect::<Vec<_>>());
    }
    Ok(vec![value])
}
fn responses(message: &ModelMessage) -> Result<Vec<Value>, ProviderError> {
    if let Some(state) = &message.provider_state {
        return state
            .payload
            .as_array()
            .cloned()
            .ok_or(ProviderError::InvalidRequest);
    }
    if message.role == ModelRole::Tool {
        return Ok(message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    call_id, content, ..
                } => {
                    Some(json!({"type":"function_call_output","call_id":call_id,"output":content}))
                }
                _ => None,
            })
            .collect());
    }
    if message.role != ModelRole::Assistant {
        return Ok(vec![
            json!({"role":role(message.role),"content":[{"type":"input_text","text":message_text(message)}]}),
        ]);
    }
    message.content.iter().map(|block| match block {
        ContentBlock::Text { text } => Ok(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})),
        ContentBlock::ToolCall { id, name, arguments } => Ok(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments})),
        _ => Err(ProviderError::InvalidRequest),
    }).collect()
}
fn anthropic(message: &ModelMessage) -> Result<Value, ProviderError> {
    if let Some(state) = &message.provider_state {
        return Ok(state.payload.clone());
    }
    if message.role == ModelRole::User {
        return Ok(json!({"role":"user","content":[{"type":"text","text":message_text(message)}]}));
    }
    let blocks = message.content.iter().map(|block| match block {
        ContentBlock::Text { text } => Ok(json!({"type":"text","text":text})),
        ContentBlock::ToolCall { id, name, arguments } => {
            let input: Value = serde_json::from_str(arguments).map_err(|_| ProviderError::InvalidRequest)?;
            if !input.is_object() { return Err(ProviderError::InvalidRequest); }
            Ok(json!({"type":"tool_use","id":id,"name":name,"input":input}))
        },
        ContentBlock::ToolResult { call_id, content, is_error } => Ok(json!({"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":is_error})),
        _ => Err(ProviderError::InvalidRequest),
    }).collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"role":if message.role == ModelRole::Assistant { "assistant" } else { "user" },"content":blocks}),
    )
}
