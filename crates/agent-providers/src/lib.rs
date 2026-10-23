//! Async HTTP transports for the runtime's canonical model protocol.
//! Credentials remain in memory and never enter model state or diagnostics.
mod encode;
mod stream;

use agent_adapters::WireProtocol;
use agent_runtime::{
    ContentBlock, ContextCapabilities, Model, ModelCapabilities, ModelContextMetadata,
    ModelFinishReason, ModelOutput, ModelRequest, ModelResponse, ModelStreamAssembler,
    ModelStreamSink, ProviderContextIdentity, ProviderReplayScope, ProviderResponseState,
    ProviderStateError, ReplayRequirement,
};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}
impl ProviderProtocol {
    pub fn wire(self) -> WireProtocol {
        match self {
            Self::ChatCompletions => WireProtocol::ChatCompletions,
            Self::Responses => WireProtocol::Responses,
            Self::AnthropicMessages => WireProtocol::AnthropicMessages,
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "messages",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningMode {
    /// No reasoning override: use the provider/model default. This does not
    /// claim to disable reasoning on models that enable it by default.
    #[default]
    Off,
    Effort {
        effort: String,
    },
    DeepSeek {
        effort: String,
    },
    AnthropicBudget {
        tokens: usize,
    },
}

/// Deliberately not serializable: persist credentials only in the host's secret store.
#[derive(Clone)]
pub struct ProviderConfig {
    pub protocol: ProviderProtocol,
    /// API root including version, e.g. https://api.openai.com/v1.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Explicit deployed-model capacity, never guessed from the model name.
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub reasoning: ReasoningMode,
    pub supports_images: bool,
}
impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("protocol", &self.protocol)
            .field("base_url", &"<redacted>")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("reasoning", &self.reasoning)
            .finish()
    }
}

/// Errors intentionally have no provider text, URLs, payloads, credentials, or sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    InvalidConfig(&'static str),
    InvalidRequest,
    ImagesUnsupported,
    InvalidResponse,
    IncompleteStream,
    Transport,
    Timeout,
    Http {
        status: u16,
        kind: HttpErrorKind,
    },
    /// A structured provider error delivered inside a successful HTTP response,
    /// including SSE. It is never automatically retried: output may have begun.
    Rejected {
        kind: HttpErrorKind,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpErrorKind {
    Authentication,
    RateLimit,
    ContextOverflow,
    Server,
    Request,
}
impl ProviderError {
    /// Retrying transport failures after output starts can duplicate generation.
    /// The client retries only explicit 429/5xx responses before opening a stream.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Http {
                kind: HttpErrorKind::RateLimit | HttpErrorKind::Server,
                ..
            }
        )
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(f, "invalid model configuration: {reason}"),
            Self::InvalidRequest => f.write_str("model request or replay state is invalid"),
            Self::ImagesUnsupported => {
                f.write_str("image input is disabled for this model configuration")
            }
            Self::InvalidResponse => f.write_str("provider returned an invalid response"),
            Self::IncompleteStream => f.write_str("provider stream ended before completion"),
            Self::Transport => f.write_str("could not connect to the model provider"),
            Self::Timeout => f.write_str("model provider request timed out"),
            Self::Http { status, kind } => write!(f, "model provider HTTP {status} ({kind:?})"),
            Self::Rejected { kind } => write!(f, "model provider rejected the response ({kind:?})"),
        }
    }
}
impl std::error::Error for ProviderError {}

#[derive(Clone)]
pub struct HttpModel {
    client: Client,
    endpoint: Url,
    protocol: ProviderProtocol,
    model: String,
    reasoning: ReasoningMode,
    metadata: ModelContextMetadata,
    identity: ProviderContextIdentity,
    supports_images: bool,
}
impl fmt::Debug for HttpModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpModel")
            .field("protocol", &self.protocol)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}
impl HttpModel {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let invalid = ProviderError::InvalidConfig;
        let mut base = Url::parse(&config.base_url).map_err(|_| invalid("invalid API base URL"))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(invalid(
                "API base URL must not contain credentials, query, or fragment",
            ));
        }
        let local = matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if base.scheme() != "https" && !(base.scheme() == "http" && local) {
            return Err(invalid("HTTPS is required except for loopback providers"));
        }
        if config.api_key.trim().is_empty()
            || config.model.trim().is_empty()
            || config.context_window == 0
            || config.max_output_tokens == 0
            || config.max_output_tokens >= config.context_window
        {
            return Err(invalid(
                "key, model, context window and output limit are required",
            ));
        }
        match &config.reasoning {
            ReasoningMode::Off => {}
            ReasoningMode::Effort { effort }
                if config.protocol != ProviderProtocol::AnthropicMessages
                    && valid_effort(effort) => {}
            ReasoningMode::DeepSeek { effort }
                if config.protocol == ProviderProtocol::ChatCompletions && valid_effort(effort) => {
            }
            ReasoningMode::AnthropicBudget { tokens }
                if config.protocol == ProviderProtocol::AnthropicMessages
                    && *tokens >= 1024
                    && *tokens < config.max_output_tokens => {}
            _ => {
                return Err(invalid(
                    "reasoning settings do not match the selected protocol or output budget",
                ));
            }
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let endpoint = base
            .join(config.protocol.endpoint())
            .map_err(|_| invalid("invalid API base URL"))?;
        let mut headers = HeaderMap::new();
        let mut secret =
            HeaderValue::from_str(&if config.protocol == ProviderProtocol::AnthropicMessages {
                config.api_key
            } else {
                format!("Bearer {}", config.api_key)
            })
            .map_err(|_| invalid("invalid API key"))?;
        secret.set_sensitive(true);
        if config.protocol == ProviderProtocol::AnthropicMessages {
            headers.insert("x-api-key", secret);
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else {
            headers.insert(AUTHORIZATION, secret);
        }
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|_| invalid("could not create HTTP client"))?;
        let identity = ProviderContextIdentity {
            provider: base.to_string(),
            model: config.model.clone(),
            format: format!(
                "{}.http.v1:{:?}",
                config.protocol.wire().as_str(),
                config.reasoning
            ),
        };
        let mut metadata =
            ModelContextMetadata::new(&identity.provider, &config.model, config.context_window)
                .map_err(|_| invalid("invalid context window"))?
                .with_accounting_identity(&identity.format);
        metadata.max_output_tokens = Some(config.max_output_tokens);
        Ok(Self {
            client,
            endpoint,
            protocol: config.protocol,
            model: config.model,
            reasoning: config.reasoning,
            metadata,
            supports_images: config.supports_images,
            identity,
        })
    }

    async fn execute(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), ProviderError> {
        let body = self.encode(&request)?;
        let mut attempt = 0;
        let response = loop {
            let response = match self
                .client
                .post(self.endpoint.clone())
                .json(&body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if error.is_connect() && attempt < 2 => {
                    // Connection establishment failed before a request could be
                    // sent. Do not retry ambiguous timeouts or interrupted streams.
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                    attempt += 1;
                    continue;
                }
                Err(error) => return Err(transport_error(error)),
            };
            if response.status().is_success() {
                break response;
            }
            let status = response.status().as_u16();
            let retry_delay = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|s| s.min(30))
                .unwrap_or(1 << attempt);
            let retryable = status == 429 || (500..=599).contains(&status);
            if retryable && attempt < 2 {
                drop(response);
                tokio::time::sleep(Duration::from_secs(retry_delay)).await;
                attempt += 1;
                continue;
            }
            let bytes = limited_body(response, 64 * 1024).await.unwrap_or_default();
            let wire = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(classify_http(status, &wire));
        };
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if content_type.contains("text/event-stream") {
            stream::consume(self, response, sink).await
        } else if content_type.contains("application/json") {
            let bytes = limited_body(response, 16 * 1024 * 1024).await?;
            let wire =
                serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse)?;
            self.decode(&wire)?.emit_stream(sink);
            Ok(())
        } else {
            Err(ProviderError::InvalidResponse)
        }
    }

    fn decode(&self, wire: &Value) -> Result<ModelOutput, ProviderError> {
        let invalid = || ProviderError::InvalidResponse;
        if let Some(error) = wire.get("error").filter(|error| !error.is_null()) {
            return Err(classify_rejection(error));
        }
        let (payload, reason) = match self.protocol {
            ProviderProtocol::ChatCompletions => {
                let choices = wire["choices"]
                    .as_array()
                    .filter(|c| c.len() == 1)
                    .ok_or_else(invalid)?;
                let reason = match choices[0]["finish_reason"].as_str() {
                    Some("stop") => ModelFinishReason::Stop,
                    Some("tool_calls") => ModelFinishReason::ToolCalls,
                    Some("length") => ModelFinishReason::Length,
                    _ => return Err(invalid()),
                };
                (choices[0]["message"].clone(), reason)
            }
            ProviderProtocol::Responses => {
                let reason = match wire["status"].as_str() {
                    Some("completed") if wire["error"].is_null() => ModelFinishReason::Stop,
                    Some("incomplete")
                        if wire["incomplete_details"]["reason"] == "max_output_tokens" =>
                    {
                        ModelFinishReason::Length
                    }
                    _ => return Err(invalid()),
                };
                (wire["output"].clone(), reason)
            }
            ProviderProtocol::AnthropicMessages => {
                let reason = match wire["stop_reason"].as_str() {
                    Some("end_turn" | "stop_sequence") => ModelFinishReason::Stop,
                    Some("tool_use") => ModelFinishReason::ToolCalls,
                    Some("max_tokens") => ModelFinishReason::Length,
                    _ => return Err(invalid()),
                };
                (
                    serde_json::json!({"role":"assistant", "content": wire["content"]}),
                    reason,
                )
            }
        };
        let id = wire["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(invalid)?;
        let mut content = self
            .protocol
            .wire()
            .decode_content(&payload, id)
            .map_err(|_| invalid())?;
        // Empty text parts do not precede a visible block in the stream.
        content.retain(|block| !matches!(block, ContentBlock::Text { text } | ContentBlock::Reasoning { text } if text.is_empty()));
        let reason = if self.protocol == ProviderProtocol::Responses
            && reason == ModelFinishReason::Stop
            && content
                .iter()
                .any(|c| matches!(c, ContentBlock::ToolCall { .. }))
        {
            ModelFinishReason::ToolCalls
        } else {
            reason
        };
        let response = ModelResponse {
            content,
            finish_reason: reason,
        };
        response.validate().map_err(|_| invalid())?;
        let usage = self
            .protocol
            .wire()
            .decode_usage(wire)
            .map_err(|_| invalid())?;
        let has_reasoning = match self.protocol {
            ProviderProtocol::ChatCompletions => {
                ["reasoning_content", "reasoning", "reasoning_text"]
                    .iter()
                    .any(|key| payload[*key].as_str().is_some_and(|s| !s.is_empty()))
            }
            ProviderProtocol::Responses => payload.as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["type"] == "reasoning" || item["type"] == "compaction")
            }),
            ProviderProtocol::AnthropicMessages => {
                payload["content"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "thinking" || item["type"] == "redacted_thinking"
                    })
                })
            }
        };
        // When a provider omits its reasoning breakdown, total output is a
        // conservative upper bound on replay overhead, not an exact count.
        // Never infer opaque signature/ciphertext token cost from byte length.
        let replay_tokens = if has_reasoning {
            let details = if self.protocol == ProviderProtocol::ChatCompletions {
                "completion_tokens_details"
            } else {
                "output_tokens_details"
            };
            wire["usage"][details]["reasoning_tokens"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .or_else(|| usage.map(|usage| usage.output_tokens))
                .ok_or_else(invalid)?
        } else {
            0
        };
        let state = self
            .protocol
            .wire()
            .capture_state(
                self.identity.clone(),
                ProviderReplayScope::Response,
                payload,
                replay_tokens,
            )
            .map_err(|_| invalid())?;
        Ok(ModelOutput {
            response,
            usage,
            provider_state: Some(state),
            provider_context: None,
            context_compacted: false,
        })
    }
}
fn valid_effort(value: &str) -> bool {
    matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh")
}
fn transport_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Transport
    }
}
async fn limited_body(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(ProviderError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn classify_http(status: u16, body: &Value) -> ProviderError {
    let kind = match status {
        401 | 403 => HttpErrorKind::Authentication,
        429 => HttpErrorKind::RateLimit,
        500..=599 => HttpErrorKind::Server,
        400 | 413
            if structured_error_kind(&body["error"]) == HttpErrorKind::ContextOverflow
                || known_context_limit_error(&body["error"]) =>
        {
            HttpErrorKind::ContextOverflow
        }
        _ => HttpErrorKind::Request,
    };
    ProviderError::Http { status, kind }
}

fn classify_rejection(error: &Value) -> ProviderError {
    ProviderError::Rejected {
        kind: structured_error_kind(error),
    }
}

/// Some Chat Completions providers (including DeepSeek) return the generic
/// invalid_request_error code for overflow. Accept only the observed complete
/// capacity-error template with consistent numeric evidence. Arbitrary messages
/// mentioning context limits must never initiate a compaction retry.
fn known_context_limit_error(error: &Value) -> bool {
    if error["code"] != "invalid_request_error"
        || (!error["type"].is_null() && error["type"] != "invalid_request_error")
    {
        return false;
    }
    let Some(message) = error["message"].as_str() else {
        return false;
    };
    let parsed = (|| {
        let message = message.strip_prefix("This model's maximum context length is ")?;
        let (limit, rest) = message.split_once(" tokens. However, you requested ")?;
        let (total, rest) = rest.split_once(" tokens (")?;
        let (input, rest) = rest.split_once(" in the messages, ")?;
        let (output, suffix) = rest.split_once(
            " in the completion). Please reduce the length of the messages or completion.",
        )?;
        if !(suffix.is_empty() || suffix.starts_with(" (request_id: ") && suffix.ends_with(')')) {
            return None;
        }
        let [limit, total, input, output] = [limit, total, input, output].map(|value| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<usize>().ok())
                .flatten()
        });
        let (limit, total, input, output) = (limit?, total?, input?, output?);
        Some(limit > 0 && total > limit && input.checked_add(output) == Some(total))
    })();
    parsed.unwrap_or(false)
}

/// Only protocol fields, never free-form provider text, determine the generic
/// error class. Some providers supply a generic `code` but a specific `type`.
fn structured_error_kind(error: &Value) -> HttpErrorKind {
    let codes = [error["code"].as_str(), error["type"].as_str()];
    if codes.iter().any(|code| {
        matches!(
            code,
            Some("context_length_exceeded" | "context_window_exceeded" | "prompt_too_long")
        )
    }) {
        HttpErrorKind::ContextOverflow
    } else if codes.iter().any(|code| {
        matches!(
            code,
            Some("rate_limit_exceeded" | "rate_limit_error" | "insufficient_quota")
        )
    }) {
        HttpErrorKind::RateLimit
    } else if codes.iter().any(|code| {
        matches!(
            code,
            Some("server_error" | "internal_server_error" | "api_error" | "overloaded_error")
        )
    }) {
        HttpErrorKind::Server
    } else if codes.iter().any(|code| {
        matches!(
            code,
            Some("authentication_error" | "invalid_api_key" | "permission_error")
        )
    }) {
        HttpErrorKind::Authentication
    } else {
        HttpErrorKind::Request
    }
}
impl Model for HttpModel {
    type Error = ProviderError;
    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        matches!(
            error,
            ProviderError::Http {
                kind: HttpErrorKind::ContextOverflow,
                ..
            } | ProviderError::Rejected {
                kind: HttpErrorKind::ContextOverflow
            }
        )
    }
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }
    fn requires_provider_state(&self) -> bool {
        self.reasoning != ReasoningMode::Off || self.protocol == ProviderProtocol::Responses
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_images: self.supports_images,
            context: ContextCapabilities {
                accepts_native_context: false,
                supports_native_compaction: false,
                replay_requirement: if self.requires_provider_state() {
                    ReplayRequirement::Required
                } else {
                    ReplayRequirement::Optional
                },
            },
            ..Default::default()
        }
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        if state.identity != self.identity || state.protocol != self.protocol.wire().as_str() {
            return Err(ProviderStateError::InvalidIdentity);
        }
        agent_adapters::validate_replay_state(state, content)
    }
    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        self.execute(request, sink).await
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(self.respond_with_metadata(request).await?.response)
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        let mut assembler = ModelStreamAssembler::new();
        self.execute(request, &mut assembler).await?;
        assembler
            .finish()
            .map_err(|_| ProviderError::InvalidResponse)
    }
}

#[cfg(test)]
mod tests;
