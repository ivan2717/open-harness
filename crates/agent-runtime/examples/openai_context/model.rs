//! Opt-in evaluation adapter; no production provider or transport is installed.
use std::{
    io,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use agent_adapters::WireProtocol;
use agent_runtime::{
    ApproximateTokenCounter, ContentBlock, MessageContext, Model, ModelContextMetadata,
    ModelFinishReason, ModelOutput, ModelRequest, ModelResponse, ProviderContextIdentity,
    ProviderReplayScope, ProviderResponseState, ProviderStateError, TokenCounter,
};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, process::Command};

#[path = "replay.rs"]
mod replay;
use replay::{ReplayStats, encode_input, validate_output};

const MODEL: &str = "gpt-6-luna";
const MAX_REQUESTS: usize = 32;
const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(180);
const DOCUMENTED_CONTEXT_WINDOW: usize = 1_050_000;
const DOCUMENTED_MAX_OUTPUT_TOKENS: usize = 128_000;
const MODEL_DOCUMENTATION: &str = "https://developers.openai.com/api/docs/models/gpt-6-luna";
const EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];

#[derive(Clone)]
pub struct OpenAiModel {
    metadata: ModelContextMetadata,
    identity: ProviderContextIdentity,
    catalog: Value,
    effort: String,
    output_cap: usize,
    requests: Arc<AtomicUsize>,
    observations: Arc<Mutex<Vec<Value>>>,
}

impl OpenAiModel {
    pub async fn discover(effort: &str) -> io::Result<Self> {
        validate_effort(effort)?;
        Self::from_catalog(transport(json!({"operation":"models"})).await?, effort)
    }

    fn from_catalog(catalog: Value, effort: &str) -> io::Result<Self> {
        validate_effort(effort)?;
        let mut entry = catalog["data"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["id"] == MODEL))
            .ok_or_else(|| io::Error::other("gpt-6-luna is absent from /models"))?
            .clone();
        let (window, window_source) =
            catalog_limit(&entry, "context_window", DOCUMENTED_CONTEXT_WINDOW)?;
        let (max_output, output_source) =
            catalog_limit(&entry, "max_output_tokens", DOCUMENTED_MAX_OUTPUT_TOKENS)?;
        let mut metadata = ModelContextMetadata::new("openai", MODEL, window)
            .map_err(io::Error::other)?
            .with_accounting_identity(format!("responses.v1:reasoning={effort}:reasoning_context=all_turns:stateless:counter=approximate.v1"));
        metadata.max_output_tokens = Some(max_output);
        metadata.validate().map_err(io::Error::other)?;
        entry["resolved_context_window"] = json!(window);
        entry["resolved_max_output_tokens"] = json!(max_output);
        entry["context_window_source"] = json!(window_source);
        entry["max_output_tokens_source"] = json!(output_source);
        entry["documentation"] = json!(MODEL_DOCUMENTATION);
        entry["reasoning_effort"] = json!(effort);
        entry["reasoning_context"] = json!("all_turns");
        entry["input_estimate_method"] = json!(
            "Local approximate counter with durable provider usage calibration; no separate counting request."
        );
        entry["replay_cost_method"] = json!(
            "Provider output_tokens_details.reasoning_tokens is a conservative estimate of additional replay input, not an exact measurement; output_tokens is the upper-bound fallback. Ciphertext length is never tokenized."
        );
        Ok(Self {
            metadata,
            identity: ProviderContextIdentity {
                provider: "openai".into(),
                model: MODEL.into(),
                format: "responses.v1:stateless".into(),
            },
            catalog: entry,
            effort: effort.into(),
            output_cap: 8192,
            requests: Arc::default(),
            observations: Arc::default(),
        })
    }

    pub fn with_output_cap(&self, output_cap: usize) -> Self {
        Self {
            output_cap,
            ..self.clone()
        }
    }

    pub fn token_counter(&self) -> ApproximateTokenCounter {
        ApproximateTokenCounter::default()
    }

    pub fn catalog(&self) -> &Value {
        &self.catalog
    }

    pub fn observations(&self) -> Vec<Value> {
        self.observations.lock().unwrap().clone()
    }

    fn encode(&self, request: &ModelRequest) -> io::Result<(Value, ReplayStats)> {
        if request.provider_context.is_some() {
            return Err(io::Error::other(
                "this evaluation does not support native context windows",
            ));
        }
        self.validate_request(request).map_err(io::Error::other)?;
        let (input, stats) = encode_input(request, &self.identity)?;
        let max_output = request
            .max_output_tokens
            .unwrap_or(8192)
            .min(self.output_cap)
            .min(self.metadata.max_output_tokens.unwrap());
        if max_output == 0 {
            return Err(io::Error::other("output cap must be positive"));
        }
        let mut body = json!({
            "model":MODEL, "instructions":request.instructions, "input":input,
            "reasoning":{"effort":self.effort,"context":"all_turns"},
            "max_output_tokens":max_output, "store":false, "stream":false,
            "include":["reasoning.encrypted_content"],
        });
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(|tool| {
                let schema: Value = serde_json::from_str(&tool.input_schema).map_err(io::Error::other)?;
                Ok(json!({"type":"function","name":tool.name,"description":tool.description,
                    "parameters":schema,"strict":false}))
            }).collect::<io::Result<Vec<_>>>()?);
        }
        Ok((body, stats))
    }

    fn decode(&self, wire: &Value) -> io::Result<ModelOutput> {
        if wire["status"] != "completed"
            || !wire["error"].is_null()
            || !wire["incomplete_details"].is_null()
        {
            return Err(io::Error::other("OpenAI response was not completed"));
        }
        if wire["reasoning"]
            .get("context")
            .is_some_and(|context| !context.is_null() && context != "all_turns")
        {
            return Err(io::Error::other(
                "OpenAI returned an incompatible reasoning context",
            ));
        }
        validate_output(&wire["output"]).map_err(io::Error::other)?;
        let response_id = wire["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| io::Error::other("OpenAI response id is missing"))?;
        let content = WireProtocol::Responses
            .decode_content(&wire["output"], response_id)
            .map_err(io::Error::other)?;
        let has_calls = content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall { .. }));
        let response = ModelResponse {
            content,
            finish_reason: if has_calls {
                ModelFinishReason::ToolCalls
            } else {
                ModelFinishReason::Stop
            },
        };
        response.validate().map_err(io::Error::other)?;
        let usage = WireProtocol::Responses
            .decode_usage(wire)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("OpenAI usage is missing"))?;
        let reasoning_items = wire["output"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .count();
        // This is additional estimated replay cost, not an exact token count.
        // Reasoning output usage may overstate retained reasoning on the next
        // request. The ciphertext's byte length cannot establish token cost.
        let replay_tokens = if reasoning_items == 0 {
            0
        } else {
            optional_token_field(&wire["usage"]["output_tokens_details"], "reasoning_tokens")?
                .unwrap_or(usage.output_tokens)
        };
        let state = WireProtocol::Responses
            .capture_state(
                self.identity.clone(),
                ProviderReplayScope::Response,
                wire["output"].clone(),
                replay_tokens,
            )
            .map_err(io::Error::other)?;
        Ok(ModelOutput {
            response,
            provider_state: Some(state),
            usage: Some(usage),
            provider_context: None,
            context_compacted: false,
        })
    }
}

impl Model for OpenAiModel {
    type Error = io::Error;

    fn capabilities(&self) -> agent_runtime::ModelCapabilities {
        agent_runtime::ModelCapabilities {
            reasoning: agent_runtime::ReasoningCapabilities {
                options: EFFORTS
                    .iter()
                    .map(|effort| agent_runtime::ReasoningOption {
                        id: (*effort).into(),
                        label: (*effort).into(),
                    })
                    .collect(),
                default_option: Some(self.effort.clone()),
            },
            context: agent_runtime::ContextCapabilities {
                replay_requirement: agent_runtime::ReplayRequirement::Required,
                ..Default::default()
            },
        }
    }

    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        Some(&self.metadata)
    }

    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        error
            .get_ref()
            .is_some_and(|source| source.is::<ContextWindowExceeded>())
    }

    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        if state.identity != self.identity || state.protocol != WireProtocol::Responses.as_str() {
            return Err(ProviderStateError::InvalidIdentity);
        }
        validate_output(&state.payload)?;
        agent_adapters::validate_replay_state(state, content)
    }

    fn requires_provider_state(&self) -> bool {
        true
    }

    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        Some(&self.identity)
    }

    async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
        Ok(self.respond_with_metadata(request).await?.response)
    }

    async fn respond_with_metadata(&self, request: ModelRequest) -> io::Result<ModelOutput> {
        let (body, replayed) = self.encode(&request)?;
        let index = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        if index > MAX_REQUESTS {
            return Err(io::Error::other("live evaluation request limit reached"));
        }
        let estimate = self.token_counter().count_tokens(&request);
        let mut portable = request.clone();
        for message in &mut portable.messages {
            message.provider_state = None;
            message
                .content
                .retain(|block| !matches!(block, ContentBlock::Reasoning { .. }));
        }
        let compactor = super::scenarios::is_summary_request(&request);
        let kind = if compactor { "compactor" } else { "agent" };
        let mut observation = json!({
            "request":index, "kind":kind, "reasoning_effort":self.effort,
            "reasoning_context":"all_turns",
            "estimated_input_tokens":estimate, "wire_estimated_input_tokens":estimate,
            "portable_estimated_input_tokens":self.token_counter().count_tokens(&portable),
            "messages":request.messages.len(), "tools":request.tools.len(),
            "input_items":body["input"].as_array().unwrap().len(),
            "has_summary":request.messages.iter().any(|m| matches!(m.context, Some(MessageContext::Summary { .. }))),
            "max_output_tokens":body["max_output_tokens"],
            "replayed_assistant_count":replayed.assistants,
            "replayed_output_items":replayed.output_items,
            "replayed_reasoning_items":replayed.reasoning_items,
            "replayed_reasoning_token_estimate":replayed.replay_tokens,
        });
        eprintln!(
            "openai request={index} kind={kind} estimate={estimate} messages={}",
            request.messages.len()
        );
        let started = Instant::now();
        let result = transport(json!({"operation":"respond","body":body})).await;
        observation["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
        let wire = match result {
            Ok(wire) => wire,
            Err(error) => {
                observation["error"] = json!(error.to_string());
                self.observations.lock().unwrap().push(observation);
                return Err(error);
            }
        };
        let result = self.decode(&wire);
        observation["completed"] = json!(wire["status"] == "completed");
        observation["response_model"] =
            safe_identifier(wire["model"].as_str(), 128).map_or(Value::Null, |model| json!(model));
        observation["effective_reasoning_effort"] =
            known_enum(&wire["reasoning"]["effort"], EFFORTS);
        observation["effective_reasoning_context"] = known_enum(
            &wire["reasoning"]["context"],
            &["all_turns", "current_turn"],
        );
        if let Some(items) = wire["output"].as_array() {
            observation["output_items"] = json!(items.len());
            observation["response_reasoning_items"] = json!(
                items
                    .iter()
                    .filter(|item| item["type"] == "reasoning")
                    .count()
            );
            observation["response_tool_calls"] = json!(
                items
                    .iter()
                    .filter(|item| item["type"] == "function_call")
                    .count()
            );
        }
        // Only numeric accounting fields enter the persistent diagnostic report.
        for field in ["input_tokens", "output_tokens", "total_tokens"] {
            observation["usage"][field] = wire["usage"][field]
                .as_u64()
                .map_or(Value::Null, |n| json!(n));
        }
        for (details, field) in [
            ("input_tokens_details", "cached_tokens"),
            ("output_tokens_details", "reasoning_tokens"),
        ] {
            observation["usage"][details][field] = wire["usage"][details][field]
                .as_u64()
                .map_or(Value::Null, |n| json!(n));
        }
        observation["returned_reasoning_tokens"] =
            observation["usage"]["output_tokens_details"]["reasoning_tokens"].clone();
        if let Err(error) = &result {
            observation["error"] = json!(error.to_string());
        }
        eprintln!(
            "openai response={index} input={} output={} completed={} elapsed_ms={}",
            observation["usage"]["input_tokens"],
            observation["usage"]["output_tokens"],
            observation["completed"],
            observation["elapsed_ms"]
        );
        self.observations.lock().unwrap().push(observation);
        result
    }
}

fn validate_effort(effort: &str) -> io::Result<()> {
    if EFFORTS.contains(&effort) {
        Ok(())
    } else {
        Err(io::Error::other("unsupported gpt-6-luna reasoning effort"))
    }
}

fn catalog_limit(entry: &Value, field: &str, fallback: usize) -> io::Result<(usize, &'static str)> {
    match optional_token_field(entry, field)? {
        Some(0) => Err(io::Error::other("catalog token limit must be positive")),
        Some(value) => Ok((value, "live_models_catalog")),
        None => Ok((fallback, "official_model_documentation_fallback")),
    }
}

fn optional_token_field(value: &Value, field: &str) -> io::Result<Option<usize>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| io::Error::other("invalid provider token count")),
    }
}

#[derive(Debug)]
struct ContextWindowExceeded;
impl std::fmt::Display for ContextWindowExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OpenAI context window exceeded (context_length_exceeded)")
    }
}
impl std::error::Error for ContextWindowExceeded {}

fn known_enum(value: &Value, allowed: &[&str]) -> Value {
    value
        .as_str()
        .filter(|value| allowed.contains(value))
        .map_or(Value::Null, |value| json!(value))
}

fn safe_identifier(value: Option<&str>, limit: usize) -> Option<&str> {
    value.filter(|value| {
        !value.is_empty()
            && value.len() <= limit
            && value.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '[' | ']')
            })
    })
}

fn transport_error(value: &Value) -> Option<io::Error> {
    let raw = value.get("transport_error")?;
    let body = raw
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| raw.clone());
    let code = value
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| body["error"]["code"].as_str());
    if value["http_status"] == 400 && code == Some("context_length_exceeded") {
        return Some(io::Error::other(ContextWindowExceeded));
    }
    let status = value["http_status"]
        .as_u64()
        .map_or_else(|| "unavailable".into(), |s| s.to_string());
    let code = safe_identifier(code, 80).unwrap_or("unclassified");
    let kind = safe_identifier(body["error"]["type"].as_str(), 80).unwrap_or("unclassified");
    let param = safe_identifier(body["error"]["param"].as_str(), 120).unwrap_or("unspecified");
    // Never print the raw server error: it may quote input or credentials.
    Some(io::Error::other(format!(
        "OpenAI HTTP {status}: code={code} type={kind} param={param}"
    )))
}

async fn transport(envelope: Value) -> io::Result<Value> {
    let mut command = Command::new("python3");
    command.arg("-c").arg(include_str!("http_bridge.py"));
    run_transport(command, envelope, TRANSPORT_TIMEOUT).await
}

async fn run_transport(
    mut command: Command,
    envelope: Value,
    deadline: Duration,
) -> io::Result<Value> {
    // Keep ownership of the process in the cancellable future. Dropping either
    // the runtime request or this timeout kills the child, including while a
    // large request is still being written to its stdin pipe.
    tokio::time::timeout(deadline, async move {
        let mut child = command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&serde_json::to_vec(&envelope)?).await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(io::Error::other("OpenAI transport process failed"));
        }
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| io::Error::other("OpenAI transport returned invalid JSON"))?;
        if let Some(error) = transport_error(&value) {
            return Err(error);
        }
        Ok(value)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "OpenAI transport timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_runtime::{ModelMessage, ToolDefinition};

    fn fixture_command(script: &str) -> Command {
        let mut command = Command::new("python3");
        command.env_clear().arg("-c").arg(script);
        command
    }

    #[cfg(unix)]
    struct WaitingProcess {
        directory: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl WaitingProcess {
        fn new() -> Self {
            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir()
                .join(format!("openai-transport-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            Self { directory }
        }

        fn command(&self) -> Command {
            let mut command = fixture_command(
                "import os, sys, time\nwith open(sys.argv[1], 'w') as file:\n    file.write(str(os.getpid()))\ntime.sleep(60)",
            );
            command.arg(self.directory.join("pid"));
            command
        }

        async fn pid(&self) -> String {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(pid) = std::fs::read_to_string(self.directory.join("pid"))
                        && pid.parse::<u32>().is_ok()
                    {
                        return pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("offline fixture child did not start")
        }
    }

    #[cfg(unix)]
    impl Drop for WaitingProcess {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[cfg(unix)]
    async fn assert_process_exited(pid: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let alive = Command::new("/bin/kill")
                    .arg("-0")
                    .arg(pid)
                    .env_clear()
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled transport left its child running");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transport_timeout_includes_stalled_stdin_and_terminates_child() {
        let child = WaitingProcess::new();
        // The fixture never reads stdin; this input is larger than a pipe can
        // buffer, so timing out must cancel the write as well as process waits.
        let error = run_transport(
            child.command(),
            json!({"payload":"x".repeat(8 * 1024 * 1024)}),
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "OpenAI transport timed out");
        assert_process_exited(&child.pid().await).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_transport_terminates_child_waiting_for_response() {
        let child = WaitingProcess::new();
        let task = tokio::spawn(run_transport(
            child.command(),
            json!({}),
            Duration::from_secs(30),
        ));
        let pid = child.pid().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_process_exited(&pid).await;
    }

    #[tokio::test]
    async fn async_transport_closes_stdin_and_keeps_process_errors_sanitized() {
        let result = run_transport(fixture_command("import json, sys\njson.load(sys.stdin)\nprint('{\"object\":\"response\",\"status\":\"completed\"}')"), json!({"synthetic":"fixture"}), Duration::from_secs(5)).await.unwrap();
        assert_eq!(result, json!({"object":"response","status":"completed"}));
        for script in [
            "import sys\nprint('private fixture text', file=sys.stderr)\nsys.exit(1)",
            "print('private fixture text: invalid JSON')",
        ] {
            let error = run_transport(fixture_command(script), json!({}), Duration::from_secs(5))
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("private fixture"));
        }
    }

    fn model() -> OpenAiModel {
        OpenAiModel::from_catalog(json!({"data":[{"id":MODEL}]}), "low").unwrap()
    }

    fn request(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            instructions: "instruction".into(),
            messages,
            tools: vec![],
            max_output_tokens: Some(64),
            provider_context: None,
        }
    }

    fn wire() -> Value {
        json!({"id":"resp_test","status":"completed","output":[
            {"id":"rs_test","type":"reasoning","summary":[],"encrypted_content":"opaque"},
            {"id":"msg_test","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"before","annotations":[]}]},
            {"id":"fc_test","type":"function_call","status":"completed","call_id":"call_one","name":"lookup","arguments":"{\"q\":1}"},
            {"id":"msg_test_2","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"after","annotations":[]}]}
        ],"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":40},"output_tokens":25,"output_tokens_details":{"reasoning_tokens":20},"total_tokens":125}})
    }

    #[test]
    fn catalog_fallback_is_explicit_and_actual_limits_win() {
        let fallback = model();
        assert_eq!(fallback.metadata.context_window, 1_050_000);
        assert_eq!(
            fallback.catalog["context_window_source"],
            "official_model_documentation_fallback"
        );
        let actual = OpenAiModel::from_catalog(
            json!({"data":[{"id":MODEL,"context_window":12345,"max_output_tokens":234}]}),
            "none",
        )
        .unwrap();
        assert_eq!(actual.metadata.context_window, 12345);
        assert_eq!(actual.metadata.max_output_tokens, Some(234));
        assert_eq!(
            actual.catalog["context_window_source"],
            "live_models_catalog"
        );
        assert_ne!(
            fallback.metadata.accounting_identity,
            actual.metadata.accounting_identity
        );
        assert!(OpenAiModel::from_catalog(json!({"data":[{"id":MODEL}]}), "ultra").is_err());
        assert!(
            OpenAiModel::from_catalog(json!({"data":[{"id":"another-model"}]}), "low").is_err()
        );
    }

    #[test]
    fn stateless_body_encodes_functions_and_keeps_summary_at_user_authority() {
        let mut request = request(vec![ModelMessage::summary("cp".into(), "history".into())]);
        request.tools.push(ToolDefinition {
            name: "lookup".into(),
            description: "Look up a value".into(),
            input_schema: r#"{"type":"object","properties":{}}"#.into(),
        });
        let (body, _) = model().with_output_cap(32).encode(&request).unwrap();
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(body["max_output_tokens"], 32);
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["input"][0]["role"], "user");
        assert!(
            body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("Historical context summary")
        );
    }

    #[test]
    fn complete_output_round_trips_after_serialized_restart_with_order_intact() {
        let original = wire();
        let output = model().decode(&original).unwrap();
        assert_eq!(output.response.finish_reason, ModelFinishReason::ToolCalls);
        let message =
            ModelMessage::assistant_blocks(output.response.content, output.provider_state);
        let restored: ModelMessage =
            serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
        let (body, replayed) = model()
            .encode(&request(vec![
                restored,
                ModelMessage::tool_result("call_one".into(), "result".into(), false),
            ]))
            .unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            &input[..4],
            original["output"].as_array().unwrap().as_slice()
        );
        assert_eq!(
            input[4],
            json!({"type":"function_call_output","call_id":"call_one","output":"result"})
        );
        assert_eq!(replayed.assistants, 1);
        assert_eq!(replayed.reasoning_items, 1);
        assert_eq!(replayed.replay_tokens, 20);
    }

    #[test]
    fn usage_cache_is_subset_and_ciphertext_size_does_not_change_replay_estimate() {
        let small = model().decode(&wire()).unwrap();
        let usage = small.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_input_tokens, Some(40));
        assert_eq!(usage.output_tokens, 25);
        let mut huge = wire();
        huge["output"][0]["encrypted_content"] = json!("x".repeat(100_000));
        let large = model().decode(&huge).unwrap();
        assert_eq!(small.provider_state.as_ref().unwrap().replay_tokens, 20);
        assert_eq!(large.provider_state.as_ref().unwrap().replay_tokens, 20);
        let count =
            |output: ModelOutput| {
                model().token_counter().count_tokens(&request(vec![
                    ModelMessage::assistant_blocks(output.response.content, output.provider_state),
                ]))
            };
        assert_eq!(count(small), count(large));
        huge["usage"]["output_tokens_details"] = json!({});
        assert_eq!(
            model()
                .decode(&huge)
                .unwrap()
                .provider_state
                .unwrap()
                .replay_tokens,
            25
        );
    }

    #[test]
    fn incomplete_unknown_and_missing_reasoning_states_fail_closed() {
        for status in ["incomplete", "failed", "in_progress", "queued", "cancelled"] {
            let mut invalid = wire();
            invalid["status"] = json!(status);
            assert!(model().decode(&invalid).is_err());
        }
        for kind in ["web_search_call", "mcp_call", "compaction", "future_item"] {
            let mut invalid = wire();
            invalid["output"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":kind}));
            assert!(model().decode(&invalid).is_err());
        }
        let mut invalid = wire();
        invalid["output"][0]
            .as_object_mut()
            .unwrap()
            .remove("encrypted_content");
        assert!(model().decode(&invalid).is_err());
        let mut invalid = wire();
        invalid["output"][2]["status"] = json!("incomplete");
        assert!(model().decode(&invalid).is_err());
        assert!(
            model()
                .encode(&request(vec![ModelMessage::assistant(
                    "legacy".into(),
                    None
                )]))
                .is_err()
        );
    }

    #[test]
    fn changed_order_and_foreign_state_are_rejected() {
        let output = model().decode(&wire()).unwrap();
        let mut message =
            ModelMessage::assistant_blocks(output.response.content, output.provider_state);
        message.content.swap(0, 1);
        assert!(model().encode(&request(vec![message.clone()])).is_err());
        message.content.swap(0, 1);
        message.provider_state.as_mut().unwrap().identity.model = "foreign".into();
        assert!(model().encode(&request(vec![message])).is_err());
    }

    #[test]
    fn effective_settings_are_sanitized_and_incompatible_context_is_rejected() {
        let mut value = wire();
        // Missing echoed settings stay unknown rather than becoming requested settings.
        assert_eq!(
            known_enum(&value["reasoning"]["effort"], EFFORTS),
            Value::Null
        );
        assert!(model().decode(&value).is_ok());
        value["reasoning"] = json!({"effort":"low","context":"all_turns"});
        assert_eq!(
            known_enum(&value["reasoning"]["effort"], EFFORTS),
            json!("low")
        );
        assert!(model().decode(&value).is_ok());
        value["reasoning"]["context"] = json!("current_turn");
        assert!(model().decode(&value).is_err());
        value["reasoning"]["context"] = json!("unknown");
        assert!(model().decode(&value).is_err());
        assert_eq!(known_enum(&json!("unknown"), EFFORTS), Value::Null);
        assert!(safe_identifier(Some("gpt-6-luna-2026-10-20"), 128).is_some());
        assert!(safe_identifier(Some("server output with input text"), 128).is_none());
        let error = json!({"http_status":400,"transport_error":json!({"error":{"code":"invalid_request_error","type":"invalid_request_error","param":"reasoning.context","message":"secret input"}}).to_string()});
        let text = transport_error(&error).unwrap().to_string();
        assert!(text.contains("param=reasoning.context"));
        assert!(text.contains("type=invalid_request_error"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn only_confirmed_context_errors_are_overflow_and_error_bodies_are_redacted() {
        let overflow = json!({"http_status":400,"transport_error":json!({"error":{"code":"context_length_exceeded","message":"secret"}}).to_string()});
        assert!(model().is_context_overflow(&transport_error(&overflow).unwrap()));
        for value in [
            json!({"http_status":400,"transport_error":"secret context_length_exceeded"}),
            json!({"http_status":429,"transport_error":json!({"error":{"code":"context_length_exceeded","message":"secret"}}).to_string()}),
            json!({"http_status":401,"transport_error":json!({"error":{"code":"invalid_api_key","message":"secret"}}).to_string()}),
        ] {
            let error = transport_error(&value).unwrap();
            assert!(!model().is_context_overflow(&error));
            assert!(!error.to_string().contains("secret"));
        }
    }
}
