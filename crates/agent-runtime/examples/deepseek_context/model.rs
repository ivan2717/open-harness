use agent_adapters::WireProtocol;
use agent_runtime::{ContentBlock, MessageContext, ModelFinishReason, ModelRole};
use std::{
    io::{self, Write},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use agent_runtime::{
    ApproximateTokenCounter, Model, ModelContextMetadata, ModelMessage, ModelOutput, ModelRequest,
    ModelResponse, ModelUsage, ProviderContextIdentity, ProviderReplayScope, ProviderResponseState,
    TokenCounter,
};
use serde_json::{Value, json};

#[path = "replay.rs"]
mod replay;
use replay::{ReplayStats, encode_messages};

const MODEL: &str = "deepseek-flash";
const MAX_REQUESTS: usize = 32;

/// Evaluation-only HTTP adapter. Thinking continuity is carried by the runtime
/// response state, so replay is independent of this adapter's process lifetime.
#[derive(Clone)]
pub struct DeepSeekModel {
    metadata: ModelContextMetadata,
    identity: ProviderContextIdentity,
    catalog: Value,
    requests: Arc<AtomicUsize>,
    observations: Arc<Mutex<Vec<Value>>>,
    output_cap: usize,
    thinking: bool,
    replay_reasoning: bool,
}

impl DeepSeekModel {
    pub async fn discover() -> io::Result<Self> {
        let catalog = transport(json!({"operation":"models"})).await?;
        let entry = catalog["data"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["id"] == MODEL))
            .ok_or_else(|| io::Error::other("deepseek-flash is absent from /models"))?
            .clone();
        let window = entry["context_window"]
            .as_u64()
            .ok_or_else(|| io::Error::other("/models did not advertise context_window"))?
            as usize;
        let mut metadata =
            ModelContextMetadata::new("deepseek", MODEL, window).map_err(io::Error::other)?;
        metadata.max_output_tokens = entry["max_output_tokens"].as_u64().map(|n| n as usize);
        metadata.validate().map_err(io::Error::other)?;
        Ok(Self {
            metadata,
            identity: provider_identity(),
            catalog: entry,
            requests: Arc::default(),
            observations: Arc::default(),
            output_cap: 8192,
            thinking: false,
            replay_reasoning: false,
        })
    }

    /// Enables thinking at high effort. `false` is an intentional negative
    /// control which drops provider reasoning between tool rounds.
    pub fn with_thinking(&self, replay: bool) -> Self {
        Self {
            thinking: true,
            replay_reasoning: replay,
            ..self.clone()
        }
    }

    pub fn token_counter(&self) -> DeepSeekTokenCounter {
        DeepSeekTokenCounter(self.clone())
    }

    pub fn with_output_cap(&self, output_cap: usize) -> Self {
        Self {
            output_cap,
            ..self.clone()
        }
    }

    pub fn catalog(&self) -> &Value {
        &self.catalog
    }

    pub fn observations(&self) -> Vec<Value> {
        self.observations.lock().unwrap().clone()
    }

    fn encode(&self, request: &ModelRequest) -> io::Result<(Value, ReplayStats)> {
        let (mut body, replayed) = encode_request(
            request,
            &self.identity,
            self.thinking && self.replay_reasoning,
        )?;
        if self.thinking {
            body["thinking"] = json!({"type":"enabled"});
            body["reasoning_effort"] = json!("high");
            // Sampling controls do not apply to thinking mode.
            body.as_object_mut().unwrap().remove("temperature");
        }
        Ok((body, replayed))
    }

    fn capture_provider_state(
        &self,
        message: &Value,
        usage: &Value,
    ) -> io::Result<Option<ProviderResponseState>> {
        if !self.thinking {
            return Ok(None);
        }
        if message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
            && message["reasoning_content"].as_str().is_none()
        {
            return Err(io::Error::other(
                "thinking tool-call response did not include reasoning_content",
            ));
        }
        // The provider reports reasoning separately. When that detail is absent,
        // total completion tokens are a conservative upper bound; ciphertext or
        // serialized payload length is never used to estimate hidden state.
        let replay_tokens = usage["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .map(|tokens| tokens as usize)
            .unwrap_or(token_field(usage, "completion_tokens")?);
        WireProtocol::ChatCompletions
            .capture_state(
                self.identity.clone(),
                ProviderReplayScope::Response,
                message.clone(),
                replay_tokens,
            )
            .map(Some)
            .map_err(io::Error::other)
    }
}

fn provider_identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "deepseek".into(),
        model: MODEL.into(),
        format: "chat.completions.v1".into(),
    }
}

/// The default counter includes durable state costs once. DeepSeek only charges
/// old reasoning when tools are present, so remove that extra cost for requests
/// without tools and for the deliberate negative replay control.
#[derive(Clone)]
pub struct DeepSeekTokenCounter(DeepSeekModel);

impl TokenCounter for DeepSeekTokenCounter {
    fn count_tokens(&self, request: &ModelRequest) -> usize {
        let counter = ApproximateTokenCounter::default();
        if self.0.thinking && self.0.replay_reasoning && !request.tools.is_empty() {
            counter.count_tokens(request)
        } else {
            counter.count_tokens(&without_provider_state(request))
        }
    }

    fn count_prefix_tokens(&self, request: &ModelRequest, message_count: usize) -> usize {
        self.count_tokens(&ModelRequest {
            messages: request.messages[..message_count.min(request.messages.len())].to_vec(),
            ..request.clone()
        })
    }
}

fn without_provider_state(request: &ModelRequest) -> ModelRequest {
    let mut request = request.clone();
    for message in &mut request.messages {
        if let ModelMessage {
            role: ModelRole::Assistant,
            provider_state,
            ..
        } = message
        {
            *provider_state = None;
            message
                .content
                .retain(|block| !matches!(block, ContentBlock::Reasoning { .. }));
        }
    }
    request
}

impl Model for DeepSeekModel {
    type Error = io::Error;

    fn capabilities(&self) -> agent_runtime::ModelCapabilities {
        agent_runtime::ModelCapabilities {
            reasoning: agent_runtime::ReasoningCapabilities {
                options: vec![agent_runtime::ReasoningOption {
                    id: "high".into(),
                    label: "High".into(),
                }],
                default_option: self.thinking.then(|| "high".into()),
            },
            context: agent_runtime::ContextCapabilities {
                replay_requirement: if self.thinking {
                    agent_runtime::ReplayRequirement::Required
                } else {
                    agent_runtime::ReplayRequirement::Optional
                },
                ..Default::default()
            },
        }
    }

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }

    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), agent_runtime::ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }

    fn requires_provider_state(&self) -> bool {
        self.thinking
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }

    async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
        Ok(self.respond_with_metadata(request).await?.response)
    }

    async fn respond_with_metadata(&self, request: ModelRequest) -> io::Result<ModelOutput> {
        let index = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        if index > MAX_REQUESTS {
            return Err(io::Error::other("live evaluation request limit reached"));
        }
        let portable_estimate =
            ApproximateTokenCounter::default().count_tokens(&without_provider_state(&request));
        let estimate = self.token_counter().count_tokens(&request);
        let summary_present = request.messages.iter().any(|message| {
            let candidate = message;
            matches!(&candidate.context, Some(MessageContext::Summary { .. }))
        });
        let compactor = request
            .instructions
            .starts_with("Summarize the supplied run events");
        let (mut body, replayed) = self.encode(&request)?;
        body["max_tokens"] = json!(
            request
                .max_output_tokens
                .unwrap_or(8192)
                .min(self.output_cap)
        );
        eprintln!(
            "deepseek request={index} kind={} estimate={estimate} messages={} summary={summary_present}",
            if compactor { "compactor" } else { "agent" },
            request.messages.len()
        );
        let started = Instant::now();
        let result = transport(json!({"operation":"chat","body":body})).await;
        let mut observation = json!({
            "request":index, "kind":if compactor {"compactor"} else {"agent"},
            "estimated_input_tokens":estimate,
            "portable_estimated_input_tokens":portable_estimate,
            "wire_estimated_input_tokens":estimate,
            "thinking":self.thinking,
            "reasoning_effort":if self.thinking {Some("high")} else {None},
            "reasoning_replay_enabled":self.thinking && self.replay_reasoning,
            "replayed_assistant_count":replayed.assistants,
            "replayed_reasoning_chars":replayed.reasoning_chars,
            "replayed_reasoning_contains_archive_pin":replayed.reasoning_contains_archive_pin,
            "encoded_messages_contains_archive_pin":
                body["messages"].to_string().contains("CEDAR-7Q9"),
            "has_summary":summary_present,
            "messages":request.messages.len(), "max_output_tokens":body["max_tokens"],
            "elapsed_ms":started.elapsed().as_millis() as u64,
            "tool_names":request.tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(),
        });
        let wire = match result {
            Ok(wire) => wire,
            Err(error) => {
                observation["error"] = json!(error.to_string());
                self.observations.lock().unwrap().push(observation);
                return Err(error);
            }
        };
        let choice = &wire["choices"][0];
        observation["usage"] = wire["usage"].clone();
        observation["returned_reasoning_tokens"] =
            wire["usage"]["completion_tokens_details"]["reasoning_tokens"].clone();
        observation["response_reasoning_chars"] = json!(
            choice["message"]["reasoning_content"]
                .as_str()
                .map_or(0, |reasoning| reasoning.chars().count())
        );
        observation["finish_reason"] = choice["finish_reason"].clone();
        observation["model"] = wire["model"].clone();
        observation["returned_tool_names"] = json!(
            choice["message"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|call| call["function"]["name"].as_str())
                .collect::<Vec<_>>()
        );
        eprintln!(
            "deepseek response={index} input={} output={} finish={} elapsed_ms={}",
            wire["usage"]["prompt_tokens"],
            wire["usage"]["completion_tokens"],
            choice["finish_reason"],
            observation["elapsed_ms"]
        );
        self.observations.lock().unwrap().push(observation);
        if !matches!(
            choice["finish_reason"].as_str(),
            Some("stop" | "tool_calls")
        ) {
            return Err(io::Error::other(format!(
                "incomplete DeepSeek response: finish_reason={}",
                choice["finish_reason"]
            )));
        }
        let message = &choice["message"];
        let fallback_response_id = format!("deepseek-response-{index}");
        let response_namespace = wire["id"].as_str().unwrap_or(&fallback_response_id);
        let response = ModelResponse {
            content: WireProtocol::ChatCompletions
                .decode_content(message, response_namespace)
                .map_err(io::Error::other)?,
            finish_reason: if choice["finish_reason"] == "tool_calls" {
                ModelFinishReason::ToolCalls
            } else {
                ModelFinishReason::Stop
            },
        };
        let provider_state = self.capture_provider_state(message, &wire["usage"])?;
        Ok(ModelOutput {
            response,
            provider_state,
            usage: Some(ModelUsage {
                input_tokens: token_field(&wire["usage"], "prompt_tokens")?,
                output_tokens: token_field(&wire["usage"], "completion_tokens")?,
                cached_input_tokens: wire["usage"]["prompt_cache_hit_tokens"]
                    .as_u64()
                    .map(|n| n as usize),
            }),
            provider_context: None,
            context_compacted: false,
        })
    }
}

fn token_field(value: &Value, field: &str) -> io::Result<usize> {
    value[field]
        .as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| io::Error::other(format!("missing usage field {field}")))
}

fn encode_request(
    request: &ModelRequest,
    identity: &ProviderContextIdentity,
    replay_reasoning: bool,
) -> io::Result<(Value, ReplayStats)> {
    if request.provider_context.is_some() {
        return Err(io::Error::other(
            "this text-mode evaluation does not support native windows",
        ));
    }
    let (messages, replayed) = encode_messages(request, identity, replay_reasoning)?;
    let mut body = json!({
        "model":MODEL, "messages":messages, "thinking":{"type":"disabled"},
        "temperature":0, "stream":false,
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    let schema: Value =
                        serde_json::from_str(&tool.input_schema).map_err(io::Error::other)?;
                    Ok(json!({"type":"function","function":{
                        "name":tool.name,"description":tool.description,"parameters":schema,
                    }}))
                })
                .collect::<io::Result<Vec<_>>>()?,
        );
    }
    Ok((body, replayed))
}

async fn transport(envelope: Value) -> io::Result<Value> {
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/deepseek_context/http_bridge.py"
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(serde_json::to_string(&envelope)?.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io::Error::other("DeepSeek transport process failed"));
        }
        let value: Value = serde_json::from_slice(&output.stdout)?;
        if let Some(error) = value.get("transport_error") {
            return Err(io::Error::other(format!(
                "DeepSeek HTTP {}: {error}",
                value["http_status"]
            )));
        }
        Ok(value)
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_runtime::ToolDefinition;

    fn offline_model() -> DeepSeekModel {
        DeepSeekModel {
            metadata: ModelContextMetadata::new("deepseek", MODEL, 1_000_000).unwrap(),
            identity: provider_identity(),
            catalog: json!({}),
            requests: Arc::default(),
            observations: Arc::default(),
            output_cap: 8192,
            thinking: false,
            replay_reasoning: false,
        }
    }

    fn wire_assistant() -> Value {
        json!({
            "role":"assistant","content":null,"reasoning_content":"private CEDAR-7Q9 思考",
            "tool_calls":[{"id":"provider-call-1","type":"function",
                "function":{"name":"lookup","arguments":"{}"}}],
            "provider_extension":{"must_survive":true}
        })
    }

    fn tool_request() -> ModelRequest {
        let state = WireProtocol::ChatCompletions
            .capture_state(
                provider_identity(),
                ProviderReplayScope::Response,
                wire_assistant(),
                37,
            )
            .unwrap();
        ModelRequest {
            instructions: "Fixed instructions".into(),
            messages: vec![
                ModelMessage::user("Please look this up".into()),
                ModelMessage::assistant_blocks(
                    WireProtocol::ChatCompletions
                        .decode_content(&state.payload, "fixture-response")
                        .unwrap(),
                    Some(state),
                ),
                ModelMessage::tool_result("provider-call-1".into(), "Found".into(), false),
            ],
            tools: vec![ToolDefinition {
                name: "lookup".into(),
                description: "Lookup records".into(),
                input_schema: "{}".into(),
            }],
            max_output_tokens: Some(8192),
            provider_context: None,
        }
    }

    #[test]
    fn counter_uses_durable_reasoning_cost_once_and_no_tools_ignore_it() {
        let model = offline_model().with_thinking(true);
        let counter = model.token_counter();
        let request = tool_request();
        let base = ApproximateTokenCounter::default();
        assert_eq!(
            counter.count_tokens(&request),
            base.count_tokens(&without_provider_state(&request)) + 37
        );
        assert_eq!(
            counter.count_prefix_tokens(&request, 1),
            base.count_prefix_tokens(&request, 1)
        );
        assert_eq!(
            counter.count_prefix_tokens(&request, 2),
            base.count_prefix_tokens(&without_provider_state(&request), 2) + 37
        );
        let mut without_tools = request.clone();
        without_tools.tools.clear();
        assert_eq!(
            counter.count_tokens(&without_tools),
            base.count_tokens(&without_provider_state(&without_tools))
        );
        // DeepSeek ignores this exact replayed reasoning when no tools are sent.
        let (wire, stats) = model.encode(&without_tools).unwrap();
        assert_eq!(wire["messages"][2], wire_assistant());
        assert_eq!(stats.assistants, 1);
    }

    #[test]
    fn fresh_adapter_replays_serialized_state_exactly_without_duplicate_tool_calls() {
        let mut request = tool_request();
        if let ModelMessage {
            role: ModelRole::Assistant,
            provider_state,
            ..
        } = &mut request.messages[1]
        {
            let bytes = serde_json::to_vec(provider_state).unwrap();
            *provider_state = serde_json::from_slice(&bytes).unwrap();
        }
        let fresh_model = offline_model().with_thinking(true);
        let (body, stats) = fresh_model.encode(&request).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["messages"][2], wire_assistant());
        assert_eq!(body["messages"].as_array().unwrap().len(), 4);
        assert_eq!(
            body["messages"][2]["tool_calls"].as_array().unwrap().len(),
            1
        );
        assert_eq!(stats.assistants, 1);
        assert!(stats.reasoning_contains_archive_pin);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn compaction_drops_old_state_and_negative_control_drops_replay() {
        let model = offline_model().with_thinking(true);
        let mut compacted = tool_request();
        compacted.messages = vec![ModelMessage::summary(
            "summary-1".into(),
            "Retained factual summary".into(),
        )];
        let (body, stats) = model.encode(&compacted).unwrap();
        assert_eq!(stats.assistants, 0);
        assert!(!body["messages"].to_string().contains("CEDAR-7Q9"));

        let negative = model.with_thinking(false);
        let (body, stats) = negative.encode(&tool_request()).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(stats.assistants, 0);
        assert!(body["messages"][2].get("reasoning_content").is_none());
        assert_eq!(
            negative.token_counter().count_tokens(&tool_request()),
            ApproximateTokenCounter::default()
                .count_tokens(&without_provider_state(&tool_request()))
        );
    }

    #[test]
    fn replay_rejects_missing_foreign_or_modified_state() {
        let model = offline_model().with_thinking(true);
        let missing = without_provider_state(&tool_request());
        assert!(model.encode(&missing).is_err());
        assert!(model.with_thinking(false).encode(&missing).is_ok());

        let mut foreign = tool_request();
        if let ModelMessage {
            role: ModelRole::Assistant,
            provider_state,
            ..
        } = &mut foreign.messages[1]
        {
            provider_state.as_mut().unwrap().identity.model = "other-model".into();
        }
        assert!(model.encode(&foreign).is_err());

        let mut changed_content = tool_request();
        if let ModelMessage {
            role: ModelRole::Assistant,
            content,
            ..
        } = &mut changed_content.messages[1]
        {
            content.push(ContentBlock::Text {
                text: "Changed".into(),
            });
        }
        assert!(model.encode(&changed_content).is_err());
        let mut changed_arguments = tool_request();
        if let ContentBlock::ToolCall { arguments, .. } =
            &mut changed_arguments.messages[1].content[1]
        {
            *arguments = "{\"changed\":true}".into();
        }
        assert!(model.encode(&changed_arguments).is_err());
    }

    #[test]
    fn every_thinking_response_keeps_full_state_and_provider_token_cost() {
        let model = offline_model().with_thinking(true);
        let final_answer = json!({
            "role":"assistant", "content":"Done", "reasoning_content":"private reasoning",
            "provider_extension":{"exact":"value"}
        });
        for message in [wire_assistant(), final_answer] {
            let state = model
                .capture_provider_state(
                    &message,
                    &json!({"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":41}}),
                )
                .unwrap()
                .unwrap();
            assert_eq!(state.payload, message);
            assert_eq!(state.replay_tokens, 41);
            let conservative = model
                .capture_provider_state(&message, &json!({"completion_tokens":100}))
                .unwrap()
                .unwrap();
            assert_eq!(conservative.replay_tokens, 100);
        }
    }

    #[test]
    fn nonthinking_wire_behavior_is_unchanged_and_identity_is_explicit() {
        let model = offline_model();
        let request = tool_request();
        let (body, stats) = model.encode(&request).unwrap();
        assert_eq!(
            body,
            encode_request(&request, &provider_identity(), false)
                .unwrap()
                .0
        );
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["temperature"], 0);
        assert_eq!(stats.assistants, 0);
        assert!(!model.requires_provider_state());
        assert!(model.with_thinking(true).requires_provider_state());
        assert_eq!(
            model.provider_context_identity(),
            Some(&provider_identity())
        );
        assert!(
            model
                .capture_provider_state(&wire_assistant(), &json!({"completion_tokens":100}))
                .unwrap()
                .is_none()
        );
    }
}
