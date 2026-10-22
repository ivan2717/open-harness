//! Bounded SSE framing and protocol-specific settlement. The response owns the
//! socket; cancelling/dropping this future drops the in-flight transport.
use super::*;
use agent_runtime::{ModelBlockKind, ModelStreamEvent};
use serde_json::json;
use std::collections::BTreeMap;

const MAX_EVENT: usize = 4 * 1024 * 1024;
const MAX_RESPONSE: usize = 32 * 1024 * 1024;

pub(super) async fn consume(
    model: &HttpModel,
    mut response: reqwest::Response,
    sink: &mut dyn ModelStreamSink,
) -> Result<(), ProviderError> {
    let mut parser = Sse::default();
    let mut state = State::new(model.protocol);
    let mut live = Live::default();
    let mut total = 0usize;
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        total = total.saturating_add(chunk.len());
        if total > MAX_RESPONSE {
            return Err(ProviderError::InvalidResponse);
        }
        for data in parser.push(&chunk)? {
            state.event(&data, &mut live, sink)?;
            if state.done {
                let output = model.decode(&state.finish()?)?;
                live.settle(output, sink)?;
                return Ok(());
            }
        }
    }
    Err(ProviderError::IncompleteStream)
}

#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
    data: Vec<u8>,
}
impl Sse {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        self.pending.extend_from_slice(bytes);
        let mut events = vec![];
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line: Vec<_> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    events.push(
                        String::from_utf8(std::mem::take(&mut self.data))
                            .map_err(|_| ProviderError::InvalidResponse)?,
                    );
                }
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            }
            if self.data.len() > MAX_EVENT {
                return Err(ProviderError::InvalidResponse);
            }
        }
        if self.pending.len() > MAX_EVENT {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(events)
    }
}

#[derive(Default)]
struct Live {
    blocks: Vec<LiveBlock>,
}
struct LiveBlock {
    key: String,
    kind: ModelBlockKind,
    text: String,
}
impl Live {
    fn start(
        &mut self,
        key: String,
        kind: ModelBlockKind,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<usize, ProviderError> {
        if let Some(index) = self.blocks.iter().position(|b| b.key == key) {
            if self.blocks[index].kind != kind {
                return Err(ProviderError::InvalidResponse);
            }
            return Ok(index);
        }
        let index = self.blocks.len();
        self.blocks.push(LiveBlock {
            key,
            kind,
            text: String::new(),
        });
        sink.emit(ModelStreamEvent::BlockStart { index, kind });
        Ok(index)
    }
    fn delta(
        &mut self,
        key: String,
        kind: ModelBlockKind,
        text: &str,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        if text.is_empty() {
            return Ok(());
        }
        let index = self.start(key, kind, sink)?;
        self.blocks[index].text.push_str(text);
        sink.emit(match kind {
            ModelBlockKind::Text => ModelStreamEvent::TextDelta {
                index,
                text: text.into(),
            },
            ModelBlockKind::Reasoning => ModelStreamEvent::ReasoningDelta {
                index,
                text: text.into(),
            },
            _ => return Err(ProviderError::InvalidResponse),
        });
        Ok(())
    }
    fn tool_delta(
        &mut self,
        key: String,
        arguments: &str,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        let index = self.start(key, ModelBlockKind::ToolCall, sink)?;
        self.blocks[index].text.push_str(arguments);
        sink.emit(ModelStreamEvent::ToolCallDelta {
            index,
            id: None,
            name: None,
            arguments_delta: arguments.into(),
        });
        Ok(())
    }
    fn settle(
        self,
        output: ModelOutput,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        // Verify the entire prefix before emitting any settlement. Unexpected
        // provider reordering never makes a partial tool call executable.
        let content = output.response.content;
        if self.blocks.len() > content.len() {
            return Err(ProviderError::InvalidResponse);
        }
        for (live, block) in self.blocks.iter().zip(&content) {
            let (kind, text) = block_parts(block);
            if kind != live.kind
                || (kind != ModelBlockKind::ToolCall && text != live.text)
                || matches!(block, ContentBlock::ToolCall { arguments, .. } if !live.text.is_empty() && arguments != &live.text)
            {
                return Err(ProviderError::InvalidResponse);
            }
        }
        for (index, block) in content.into_iter().enumerate() {
            if index >= self.blocks.len() {
                sink.emit(ModelStreamEvent::BlockStart {
                    index,
                    kind: block_parts(&block).0,
                });
            }
            sink.emit(ModelStreamEvent::BlockEnd { index, block });
        }
        if let Some(usage) = output.usage {
            sink.emit(ModelStreamEvent::Usage { usage });
        }
        sink.emit(ModelStreamEvent::Finish {
            finish_reason: output.response.finish_reason,
            provider_state: output.provider_state.map(Box::new),
            provider_context: None,
            context_compacted: false,
        });
        Ok(())
    }
}
fn block_parts(block: &ContentBlock) -> (ModelBlockKind, &str) {
    match block {
        ContentBlock::Text { text } => (ModelBlockKind::Text, text),
        ContentBlock::Reasoning { text } => (ModelBlockKind::Reasoning, text),
        ContentBlock::ToolCall { .. } => (ModelBlockKind::ToolCall, ""),
        ContentBlock::ToolResult { .. } => (ModelBlockKind::ToolResult, ""),
    }
}

struct State {
    protocol: ProviderProtocol,
    wire: Value,
    calls: BTreeMap<usize, Value>,
    partial_json: BTreeMap<usize, String>,
    open_blocks: Vec<usize>,
    done: bool,
}
impl State {
    fn new(protocol: ProviderProtocol) -> Self {
        Self {
            protocol,
            wire: json!({}),
            calls: BTreeMap::new(),
            partial_json: BTreeMap::new(),
            open_blocks: vec![],
            done: false,
        }
    }
    fn event(
        &mut self,
        data: &str,
        live: &mut Live,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        if data == "[DONE]" {
            if self.protocol != ProviderProtocol::ChatCompletions
                || self.wire["choices"][0]["finish_reason"].is_null()
            {
                return Err(ProviderError::IncompleteStream);
            }
            self.done = true;
            return Ok(());
        }
        let event: Value =
            serde_json::from_str(data).map_err(|_| ProviderError::InvalidResponse)?;
        if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
            return Err(classify_rejection(error));
        }
        if event["type"] == "error" {
            return Err(classify_rejection(&event));
        }
        match self.protocol {
            ProviderProtocol::ChatCompletions => self.chat(event, live, sink),
            ProviderProtocol::Responses => self.responses(event, live, sink),
            ProviderProtocol::AnthropicMessages => self.anthropic(event, live, sink),
        }
    }
    fn chat(
        &mut self,
        event: Value,
        live: &mut Live,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        if self.wire.get("choices").is_none() {
            self.wire = json!({"choices":[{"message":{"role":"assistant"},"finish_reason":null}]});
        }
        if let Some(id) = event.get("id") {
            self.wire["id"] = id.clone();
        }
        if event.get("usage").is_some_and(|v| !v.is_null()) {
            self.wire["usage"] = event["usage"].clone();
        }
        let choices = event["choices"]
            .as_array()
            .ok_or(ProviderError::InvalidResponse)?;
        if choices.len() > 1 {
            return Err(ProviderError::InvalidResponse);
        }
        let Some(choice) = choices.first() else {
            return Ok(());
        };
        if choice["index"].as_u64().is_some_and(|n| n != 0) {
            return Err(ProviderError::InvalidResponse);
        }
        let delta = choice["delta"]
            .as_object()
            .ok_or(ProviderError::InvalidResponse)?;
        let mut fields: Vec<_> = delta.iter().collect();
        fields.sort_by_key(|(key, _)| match key.as_str() {
            "reasoning_content" => 0,
            "reasoning" => 1,
            "reasoning_text" => 2,
            "content" => 3,
            "refusal" => 4,
            "tool_calls" => 5,
            _ => 6,
        });
        for (key, value) in fields {
            if key == "tool_calls" {
                for call in value.as_array().ok_or(ProviderError::InvalidResponse)? {
                    let index = index(call, "index")?;
                    if index > self.calls.len() {
                        return Err(ProviderError::InvalidResponse);
                    }
                    live.start(format!("tool:{index}"), ModelBlockKind::ToolCall, sink)?;
                    if let Some(arguments) = call["function"]["arguments"].as_str() {
                        live.tool_delta(format!("tool:{index}"), arguments, sink)?;
                    }
                    let target = self.calls.entry(index).or_insert_with(|| json!({}));
                    let mut item = call.clone();
                    item.as_object_mut().unwrap().remove("index");
                    merge_delta(target, &item)?;
                }
            } else {
                if matches!(
                    key.as_str(),
                    "content" | "refusal" | "reasoning_content" | "reasoning" | "reasoning_text"
                ) && !value.is_null()
                {
                    let text = value.as_str().ok_or(ProviderError::InvalidResponse)?;
                    let kind = if key.starts_with("reasoning") {
                        ModelBlockKind::Reasoning
                    } else {
                        ModelBlockKind::Text
                    };
                    live.delta(key.clone(), kind, text, sink)?;
                }
                if key == "role" {
                    self.wire["choices"][0]["message"][key] = value.clone();
                } else {
                    merge_delta(&mut self.wire["choices"][0]["message"][key], value)?;
                }
            }
        }
        if !choice["finish_reason"].is_null() {
            self.wire["choices"][0]["finish_reason"] = choice["finish_reason"].clone();
        }
        Ok(())
    }
    fn responses(
        &mut self,
        event: Value,
        live: &mut Live,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        let kind = event["type"]
            .as_str()
            .ok_or(ProviderError::InvalidResponse)?;
        match kind {
            "response.completed" | "response.incomplete" => {
                self.wire = event["response"].clone();
                self.done = true;
            }
            "response.failed" | "response.error" => {
                let error = event["response"]
                    .get("error")
                    .filter(|error| !error.is_null());
                return Err(error.map_or(ProviderError::InvalidResponse, classify_rejection));
            }
            "response.output_item.added" if event["item"]["type"] == "function_call" => {
                live.start(
                    format!("item:{}", index(&event, "output_index")?),
                    ModelBlockKind::ToolCall,
                    sink,
                )?;
            }
            "response.function_call_arguments.delta" => {
                live.tool_delta(
                    format!("item:{}", index(&event, "output_index")?),
                    event["delta"]
                        .as_str()
                        .ok_or(ProviderError::InvalidResponse)?,
                    sink,
                )?;
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta" => {
                let output = index(&event, "output_index")?;
                let part = event["content_index"]
                    .as_u64()
                    .or_else(|| event["summary_index"].as_u64())
                    .unwrap_or(0);
                let reasoning = kind.contains("reasoning");
                live.delta(
                    format!(
                        "item:{output}:{}:{part}",
                        if kind.contains("summary") {
                            "summary"
                        } else if reasoning {
                            "reasoning"
                        } else {
                            "text"
                        }
                    ),
                    if reasoning {
                        ModelBlockKind::Reasoning
                    } else {
                        ModelBlockKind::Text
                    },
                    event["delta"]
                        .as_str()
                        .ok_or(ProviderError::InvalidResponse)?,
                    sink,
                )?;
            }
            _ => {}
        }
        Ok(())
    }
    fn anthropic(
        &mut self,
        event: Value,
        live: &mut Live,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        match event["type"]
            .as_str()
            .ok_or(ProviderError::InvalidResponse)?
        {
            "message_start" => {
                if !self.wire.is_object() || !self.wire.as_object().unwrap().is_empty() {
                    return Err(ProviderError::InvalidResponse);
                }
                self.wire = event["message"].clone();
                if self.wire["role"] != "assistant" || !self.wire["content"].is_array() {
                    return Err(ProviderError::InvalidResponse);
                }
            }
            "content_block_start" => {
                let i = index(&event, "index")?;
                let blocks = self.wire["content"]
                    .as_array_mut()
                    .ok_or(ProviderError::InvalidResponse)?;
                if i != blocks.len() {
                    return Err(ProviderError::InvalidResponse);
                }
                let block = event["content_block"].clone();
                let key = format!("block:{i}");
                match block["type"].as_str() {
                    Some("tool_use") => {
                        live.start(key, ModelBlockKind::ToolCall, sink)?;
                    }
                    Some("text") => live.delta(
                        key,
                        ModelBlockKind::Text,
                        block["text"].as_str().unwrap_or(""),
                        sink,
                    )?,
                    Some("thinking") => live.delta(
                        key,
                        ModelBlockKind::Reasoning,
                        block["thinking"].as_str().unwrap_or(""),
                        sink,
                    )?,
                    _ => {}
                }
                blocks.push(block);
                self.open_blocks.push(i);
            }
            "content_block_delta" => {
                let i = index(&event, "index")?;
                if !self.open_blocks.contains(&i) {
                    return Err(ProviderError::InvalidResponse);
                }
                let delta = &event["delta"];
                let key = format!("block:{i}");
                let (field, kind) = match delta["type"].as_str() {
                    Some("text_delta") => ("text", Some(ModelBlockKind::Text)),
                    Some("thinking_delta") => ("thinking", Some(ModelBlockKind::Reasoning)),
                    Some("signature_delta") => ("signature", None),
                    Some("input_json_delta") => {
                        self.partial_json.entry(i).or_default().push_str(
                            delta["partial_json"]
                                .as_str()
                                .ok_or(ProviderError::InvalidResponse)?,
                        );
                        return Ok(());
                    }
                    Some("citations_delta") => {
                        let block = &mut self.wire["content"][i];
                        if block.get("citations").is_none() {
                            block["citations"] = json!([]);
                        }
                        block["citations"]
                            .as_array_mut()
                            .ok_or(ProviderError::InvalidResponse)?
                            .push(delta["citation"].clone());
                        return Ok(());
                    }
                    _ => return Err(ProviderError::InvalidResponse),
                };
                let text = delta[field]
                    .as_str()
                    .ok_or(ProviderError::InvalidResponse)?;
                if let Some(kind) = kind {
                    live.delta(key, kind, text, sink)?;
                }
                merge_delta(&mut self.wire["content"][i][field], &json!(text))?;
            }
            "content_block_stop" => {
                let i = index(&event, "index")?;
                let pos = self
                    .open_blocks
                    .iter()
                    .position(|n| *n == i)
                    .ok_or(ProviderError::InvalidResponse)?;
                self.open_blocks.remove(pos);
                if let Some(input) = self.partial_json.remove(&i) {
                    self.wire["content"][i]["input"] =
                        serde_json::from_str(&input).map_err(|_| ProviderError::InvalidResponse)?;
                }
            }
            "message_delta" => {
                if let Some(fields) = event["delta"].as_object() {
                    for (key, value) in fields {
                        self.wire[key] = value.clone();
                    }
                }
                if let Some(fields) = event["usage"].as_object() {
                    for (key, value) in fields {
                        self.wire["usage"][key] = value.clone();
                    }
                }
            }
            "message_stop" => {
                if !self.open_blocks.is_empty() {
                    return Err(ProviderError::IncompleteStream);
                }
                self.done = true;
            }
            "ping" => {}
            _ => return Err(ProviderError::InvalidResponse),
        }
        Ok(())
    }
    fn finish(mut self) -> Result<Value, ProviderError> {
        if !self.done {
            return Err(ProviderError::IncompleteStream);
        }
        if self.protocol == ProviderProtocol::ChatCompletions && !self.calls.is_empty() {
            self.wire["choices"][0]["message"]["tool_calls"] =
                Value::Array(self.calls.into_values().collect());
        }
        Ok(self.wire)
    }
}
fn index(value: &Value, field: &str) -> Result<usize, ProviderError> {
    value[field]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(ProviderError::InvalidResponse)
}
fn merge_delta(target: &mut Value, delta: &Value) -> Result<(), ProviderError> {
    if delta.is_null() {
        return Ok(());
    }
    if target.is_null() {
        *target = delta.clone();
        return Ok(());
    }
    match (target, delta) {
        (Value::String(target), Value::String(delta)) => target.push_str(delta),
        (Value::Object(target), Value::Object(delta)) => {
            for (key, value) in delta {
                if key == "type" {
                    let field = target.entry(key).or_insert(Value::Null);
                    if field.is_null() || *field == *value {
                        *field = value.clone();
                    } else {
                        return Err(ProviderError::InvalidResponse);
                    }
                } else {
                    merge_delta(target.entry(key).or_insert(Value::Null), value)?;
                }
            }
        }
        (target, delta) if *target == *delta => {}
        _ => return Err(ProviderError::InvalidResponse),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_handles_split_unicode_crlf_comments_and_multiline_data() {
        let mut parser = Sse::default();
        let data = ": ping\r\ndata: {\"text\":\"汉字\",\r\ndata: \"ok\":true}\r\n\r\n";
        let mut events = vec![];
        for byte in data.as_bytes() {
            events.extend(parser.push(&[*byte]).unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&events[0]).unwrap()["text"],
            "汉字"
        );
    }
}
