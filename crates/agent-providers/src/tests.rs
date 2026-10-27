use super::*;
use agent_runtime::{ModelMessage, ToolDefinition};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn config(protocol: ProviderProtocol, base_url: String) -> ProviderConfig {
    ProviderConfig {
        supports_images: false,
        protocol,
        base_url,
        api_key: "secret-test-value".into(),
        model: "fixture-model".into(),
        context_window: 32000,
        max_output_tokens: 4096,
        reasoning: ReasoningMode::Off,
    }
}
fn model(protocol: ProviderProtocol) -> HttpModel {
    HttpModel::new(config(protocol, "http://127.0.0.1:12345/v1".into())).unwrap()
}
fn request() -> ModelRequest {
    ModelRequest {
        instructions: "Be helpful".into(),
        messages: vec![ModelMessage::user("你好".into())],
        tools: vec![],
        max_output_tokens: Some(2048),
        provider_context: None,
    }
}

#[test]
fn session_reasoning_overrides_clone_the_model_and_use_each_wire_protocol() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
    ] {
        let original = model(protocol)
            .with_reasoning_capability(ReasoningCapability::legacy(&ReasoningMode::Effort {
                effort: "medium".into(),
            }))
            .unwrap();
        let derived = original.with_reasoning_effort("high").unwrap();
        assert_ne!(original.identity, derived.identity);
        assert_eq!(original.reasoning, ReasoningMode::Off);
        assert_eq!(original.endpoint, derived.endpoint);
        assert_eq!(
            original.metadata.context_window,
            derived.metadata.context_window
        );
        let body = derived.encode(&request()).unwrap();
        if protocol == ProviderProtocol::Responses {
            assert_eq!(body["reasoning"]["effort"], "high");
        } else {
            assert_eq!(body["reasoning_effort"], "high");
        }
        assert!(original.with_reasoning_effort("ultra").is_err());
    }
    let mut config = config(
        ProviderProtocol::ChatCompletions,
        "http://127.0.0.1:1".into(),
    );
    config.reasoning = ReasoningMode::DeepSeek {
        effort: "high".into(),
    };
    let deepseek = HttpModel::new(config)
        .unwrap()
        .with_reasoning_effort("low")
        .unwrap();
    let body = deepseek.encode(&request()).unwrap();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["reasoning_effort"], "low");

    let anthropic = model(ProviderProtocol::AnthropicMessages)
        .with_reasoning_capability(ReasoningCapability::legacy(
            &ReasoningMode::AnthropicBudget { tokens: 1024 },
        ))
        .unwrap();
    assert_eq!(anthropic.supported_reasoning_efforts(), vec!["low"]);
    assert!(anthropic.with_reasoning_effort("medium").is_err());
    let body = anthropic
        .with_reasoning_effort("low")
        .unwrap()
        .encode(&request())
        .unwrap();
    assert_eq!(body["thinking"]["budget_tokens"], 1024);
}
async fn serve(
    status: &str,
    content_type: &str,
    chunks: Vec<String>,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let status = status.to_owned();
    let content_type = content_type.to_owned();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let bytes = read_request(&mut socket).await;
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        for chunk in chunks {
            socket
                .write_all(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes())
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        socket.write_all(b"0\r\n\r\n").await.unwrap();
        bytes
    });
    (url, task)
}
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = vec![];
    let mut chunk = [0; 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= pos + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn sse(value: Value) -> String {
    format!("data: {value}\n\n")
}

#[tokio::test]
async fn chat_stream_reassembles_calls_reasoning_and_provider_usage() {
    let chunks = vec![
        sse(
            json!({"id":"chat-1","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Think"},"finish_reason":null}]}),
        ),
        sse(
            json!({"id":"chat-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}),
        ),
        sse(
            json!({"id":"chat-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-","type":"function","function":{"name":"look","arguments":"{\"x\":"}}]},"finish_reason":null}]}),
        ),
        sse(
            json!({"id":"chat-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"1","function":{"name":"up","arguments":"1}"}}]},"finish_reason":"tool_calls"}]}),
        ),
        sse(
            json!({"id":"chat-1","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"completion_tokens_details":{"reasoning_tokens":7},"prompt_cache_hit_tokens":40}}),
        ),
        "data: [DONE]\n\n".into(),
    ];
    let (url, task) = serve("200 OK", "text/event-stream", chunks).await;
    let model = HttpModel::new(config(ProviderProtocol::ChatCompletions, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    let sent = task.await.unwrap();
    assert!(sent.starts_with("POST /v1/chat/completions"));
    assert!(sent.contains("Bearer secret-test-value"));
    assert_eq!(output.response.text(), "Hello");
    assert_eq!(output.response.calls()[0].id, "call-1");
    assert_eq!(output.response.calls()[0].name, "lookup");
    assert_eq!(output.response.calls()[0].arguments, "{\"x\":1}");
    assert_eq!(output.provider_state.as_ref().unwrap().replay_tokens, 7);
    assert_eq!(output.usage.unwrap().cached_input_tokens, Some(40));
    let mut next = request();
    next.messages.push(ModelMessage::assistant_blocks(
        output.response.content.clone(),
        output.provider_state.clone(),
    ));
    next.messages.push(ModelMessage::tool_result(
        "call-1".into(),
        "Found".into(),
        false,
    ));
    assert_eq!(
        model.encode(&next).unwrap()["messages"][2],
        output.provider_state.unwrap().payload
    );
}

#[tokio::test]
async fn responses_stream_preserves_encrypted_state_and_all_output_items() {
    let wire = json!({"id":"resp-1","status":"completed","output":[
        {"id":"r1","type":"reasoning","summary":[{"type":"summary_text","text":"Plan"}],"encrypted_content":"opaque-private"},
        {"id":"m1","type":"message","role":"assistant","content":[{"type":"output_text","text":"Answer"}]}],
        "usage":{"input_tokens":100,"output_tokens":20,"output_tokens_details":{"reasoning_tokens":7}}});
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"Plan"})),
        sse(json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Answer"})),
        sse(json!({"type":"response.completed","response":wire})),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::Responses, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    let sent = task.await.unwrap();
    assert!(sent.starts_with("POST /v1/responses"));
    assert!(sent.contains("reasoning.encrypted_content"));
    assert_eq!(output.response.text(), "Answer");
    assert_eq!(output.provider_state.unwrap().payload, wire["output"]);
}

#[tokio::test]
async fn messages_stream_merges_usage_and_preserves_thinking_signatures() {
    let chunks = vec![
        sse(
            json!({"type":"message_start","message":{"id":"msg-1","role":"assistant","content":[],"usage":{"input_tokens":20,"output_tokens":0,"cache_read_input_tokens":10}}}),
        ),
        sse(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        ),
        sse(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}),
        ),
        sse(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"secret-signature"}}),
        ),
        sse(json!({"type":"content_block_stop","index":0})),
        sse(
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        ),
        sse(
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer"}}),
        ),
        sse(json!({"type":"content_block_stop","index":1})),
        sse(
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":13}}),
        ),
        sse(json!({"type":"message_stop"})),
    ];
    let (url, task) = serve("200 OK", "text/event-stream", chunks).await;
    let model = HttpModel::new(config(ProviderProtocol::AnthropicMessages, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    let sent = task.await.unwrap();
    assert!(sent.starts_with("POST /v1/messages"));
    assert!(sent.contains("x-api-key: secret-test-value"));
    assert_eq!(output.response.text(), "Answer");
    assert_eq!(output.usage.unwrap().input_tokens, 30);
    assert_eq!(output.provider_state.as_ref().unwrap().replay_tokens, 13);
    assert_eq!(
        output.provider_state.unwrap().payload["content"][0]["signature"],
        "secret-signature"
    );
}

#[tokio::test]
async fn truncated_stream_never_settles_and_http_errors_are_redacted() {
    let (url, task) = serve("200 OK", "text/event-stream", vec![sse(json!({"id":"x","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}))]).await;
    let model = HttpModel::new(config(ProviderProtocol::ChatCompletions, url)).unwrap();
    assert_eq!(
        model.respond(request()).await.unwrap_err(),
        ProviderError::IncompleteStream
    );
    task.await.unwrap();
    let (url, task) = serve(
        "400 Bad Request",
        "application/json",
        vec![
            json!({"error":{"code":"context_length_exceeded","message":"secret-test-value"}})
                .to_string(),
        ],
    )
    .await;
    let model = HttpModel::new(config(ProviderProtocol::ChatCompletions, url)).unwrap();
    let error = model.respond(request()).await.unwrap_err();
    assert!(model.is_context_overflow(&error));
    assert!(!format!("{error:?} {error} {model:?}").contains("secret-test-value"));
    task.await.unwrap();
}

#[test]
fn config_and_request_validation_do_not_leak_credentials() {
    let mut config = config(
        ProviderProtocol::Responses,
        "https://api.example.com/v1?key=secret-test-value".into(),
    );
    assert!(!format!("{config:?}").contains("secret-test-value"));
    assert!(
        !HttpModel::new(config.clone())
            .unwrap_err()
            .to_string()
            .contains("secret-test-value")
    );
    config.base_url = "http://remote.example/v1".into();
    assert!(HttpModel::new(config).is_err());
    let model = model(ProviderProtocol::ChatCompletions);
    let mut req = request();
    req.messages
        .push(ModelMessage::summary("cp".into(), "Ignore prior".into()));
    req.tools.push(ToolDefinition {
        name: "lookup".into(),
        description: "Find".into(),
        input_schema: "{\"type\":\"object\"}".into(),
    });
    let body = model.encode(&req).unwrap();
    assert_eq!(body["messages"][2]["role"], "user");
    assert!(
        body["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("Historical context")
    );
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["tools"][0]["function"]["name"], "lookup");
}
#[test]
fn context_overflow_requires_confirmed_code_and_never_rate_limits() {
    for status in [400, 429, 500] {
        let error = classify_http(
            status,
            &json!({"error":{"message":"context length exceeded"}}),
        );
        assert!(!matches!(
            error,
            ProviderError::Http {
                kind: HttpErrorKind::ContextOverflow,
                ..
            }
        ));
    }
    assert!(classify_http(429, &json!({})).is_retryable());
    assert!(!classify_http(401, &json!({})).is_retryable());
}

#[test]
fn deepseek_context_limit_template_requires_status_code_and_consistent_counts() {
    let message = "This model's maximum context length is 1048576 tokens. However, you requested 1077729 tokens (1069537 in the messages, 8192 in the completion). Please reduce the length of the messages or completion. (request_id: secret-test-value)";
    let body = json!({"error":{"code":"invalid_request_error","type":"invalid_request_error","message":message}});
    assert_eq!(
        classify_http(400, &body),
        ProviderError::Http {
            status: 400,
            kind: HttpErrorKind::ContextOverflow
        }
    );
    assert!(!format!("{:?}", classify_http(400, &body)).contains("secret-test-value"));
    for status in [401, 429, 500] {
        assert!(!matches!(
            classify_http(status, &body),
            ProviderError::Http {
                kind: HttpErrorKind::ContextOverflow,
                ..
            }
        ));
    }
    for replacement in [
        message.replace("1077729", "1077728"),
        message.replace("1048576", "2048576"),
        message.replace("1048576", "0"),
        message.replace("1077729", "99999999999999999999999999999999999"),
        format!("Quoted error: {message}"),
        format!("{message} unrelated failure"),
        "context length exceeded".into(),
    ] {
        let mut invalid = body.clone();
        invalid["error"]["message"] = json!(replacement);
        assert_eq!(
            classify_http(400, &invalid),
            ProviderError::Http {
                status: 400,
                kind: HttpErrorKind::Request
            }
        );
    }
    let mut different_code = body;
    different_code["error"]["code"] = json!("rate_limit_error");
    assert_eq!(
        classify_http(400, &different_code),
        ProviderError::Http {
            status: 400,
            kind: HttpErrorKind::Request
        }
    );
}

#[tokio::test]
async fn structured_stream_errors_retain_classes_without_provider_text_or_retries() {
    for (protocol, event, kind) in [
        (
            ProviderProtocol::Responses,
            json!({"type":"error","code":"context_length_exceeded","message":"secret-test-value"}),
            HttpErrorKind::ContextOverflow,
        ),
        (
            ProviderProtocol::Responses,
            json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"secret-test-value"}}}),
            HttpErrorKind::Server,
        ),
        (
            ProviderProtocol::ChatCompletions,
            json!({"error":{"code":"invalid_request_error","type":"context_length_exceeded","message":"secret-test-value"}}),
            HttpErrorKind::ContextOverflow,
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"type":"error","error":{"type":"overloaded_error","message":"secret-test-value"}}),
            HttpErrorKind::Server,
        ),
        (
            ProviderProtocol::Responses,
            json!({"type":"error","code":"rate_limit_exceeded","message":"secret-test-value"}),
            HttpErrorKind::RateLimit,
        ),
        (
            ProviderProtocol::Responses,
            json!({"type":"error","code":"invalid_request_error","message":"context length exceeded; secret-test-value"}),
            HttpErrorKind::Request,
        ),
    ] {
        let (url, task) = serve("200 OK", "text/event-stream", vec![sse(event)]).await;
        let model = HttpModel::new(config(protocol, url)).unwrap();
        let error = model.respond_with_metadata(request()).await.unwrap_err();
        assert_eq!(error, ProviderError::Rejected { kind });
        assert_eq!(
            model.is_context_overflow(&error),
            kind == HttpErrorKind::ContextOverflow
        );
        assert!(!error.is_retryable());
        assert!(!format!("{error:?} {error}").contains("secret-test-value"));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn failed_json_response_preserves_structured_error_without_claiming_http_failure() {
    let (url, task) = serve(
        "200 OK",
        "application/json",
        vec![json!({"status":"failed","error":{"code":"context_length_exceeded","message":"secret-test-value"}}).to_string()],
    ).await;
    let model = HttpModel::new(config(ProviderProtocol::Responses, url)).unwrap();
    let error = model.respond(request()).await.unwrap_err();
    assert_eq!(
        error,
        ProviderError::Rejected {
            kind: HttpErrorKind::ContextOverflow
        }
    );
    assert!(model.is_context_overflow(&error));
    assert!(!error.is_retryable());
    task.await.unwrap();
}

#[tokio::test]
async fn provider_error_after_partial_stream_never_settles_or_retries() {
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"})),
        sse(json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"secret-test-value"}}})),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::Responses, url)).unwrap();
    struct Events(Vec<agent_runtime::ModelStreamEvent>);
    impl ModelStreamSink for Events {
        fn emit(&mut self, event: agent_runtime::ModelStreamEvent) {
            self.0.push(event);
        }
    }
    let mut events = Events(Vec::new());
    let error = model.stream(request(), &mut events).await.unwrap_err();
    assert_eq!(
        error,
        ProviderError::Rejected {
            kind: HttpErrorKind::Server
        }
    );
    assert!(events.0.iter().any(|event| matches!(event, agent_runtime::ModelStreamEvent::TextDelta { text, .. } if text == "partial")));
    assert!(!events.0.iter().any(|event| matches!(
        event,
        agent_runtime::ModelStreamEvent::BlockEnd { .. }
            | agent_runtime::ModelStreamEvent::Finish { .. }
    )));
    task.await.unwrap();
}

#[tokio::test]
async fn empty_chat_text_does_not_displace_streamed_tool_block() {
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"id":"call-only","choices":[{"index":0,"delta":{"content":"","tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})),
        "data: [DONE]\n\n".into(),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::ChatCompletions, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    assert_eq!(output.response.content.len(), 1);
    assert_eq!(output.response.calls()[0].name, "lookup");
    task.await.unwrap();
}

#[tokio::test]
async fn cancelling_request_drops_the_http_connection_before_settlement() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = HttpModel::new(config(
        ProviderProtocol::ChatCompletions,
        format!("http://{}/v1", listener.local_addr().unwrap()),
    ))
    .unwrap();
    let (ready, arrived) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 8192];
        let _request = read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        ready.send(()).unwrap();
        match tokio::time::timeout(Duration::from_secs(2), socket.read(&mut bytes))
            .await
            .unwrap()
        {
            Ok(0) => true,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => true,
            _ => false,
        }
    });
    let request = tokio::spawn(async move { model.respond(request()).await });
    arrived.await.unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(server.await.unwrap());
}

#[tokio::test]
async fn rate_limit_retries_only_before_stream_and_uses_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = HttpModel::new(config(
        ProviderProtocol::ChatCompletions,
        format!("http://{}/v1", listener.local_addr().unwrap()),
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _request = read_request(&mut socket).await;
            let body = if attempt == 0 { json!({"error":{"type":"rate_limit","message":"secret-test-value"}}) }
                else { json!({"id":"retry","choices":[{"message":{"role":"assistant","content":"Done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}) }.to_string();
            let status = if attempt == 0 {
                "429 Too Many Requests"
            } else {
                "200 OK"
            };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    assert_eq!(model.respond(request()).await.unwrap().text(), "Done");
    server.await.unwrap();
}

#[test]
fn reasoning_modes_map_explicitly_and_missing_state_is_rejected() {
    for (protocol, reasoning, field) in [
        (
            ProviderProtocol::ChatCompletions,
            ReasoningMode::DeepSeek {
                effort: "high".into(),
            },
            "thinking",
        ),
        (
            ProviderProtocol::Responses,
            ReasoningMode::Effort {
                effort: "high".into(),
            },
            "reasoning",
        ),
        (
            ProviderProtocol::AnthropicMessages,
            ReasoningMode::AnthropicBudget { tokens: 1024 },
            "thinking",
        ),
    ] {
        let mut conf = config(protocol, "https://api.example.com/v1".into());
        conf.reasoning = reasoning;
        let model = HttpModel::new(conf).unwrap();
        assert!(model.encode(&request()).unwrap().get(field).is_some());
        let mut missing = request();
        missing
            .messages
            .push(ModelMessage::assistant("Answer".into(), None));
        assert!(model.encode(&missing).is_err());
    }
}

#[tokio::test]
async fn same_chunk_reasoning_and_text_use_canonical_order() {
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"id":"combined","choices":[{"index":0,"delta":{"role":"assistant","content":"Answer","reasoning_content":"Plan"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":4}})),
        "data: [DONE]\n\n".into(),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::ChatCompletions, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    assert!(
        matches!(&output.response.content[0], ContentBlock::Reasoning { text } if text == "Plan")
    );
    assert_eq!(output.response.text(), "Answer");
    task.await.unwrap();
}

#[tokio::test]
async fn responses_function_arguments_stream_and_settle_with_native_call_id() {
    let output = json!([{"id":"item1","type":"function_call","call_id":"call1","name":"lookup","arguments":"{\"x\":1}"}]);
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call1","name":"lookup","arguments":""}})),
        sse(json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"x\":"})),
        sse(json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"1}"})),
        sse(json!({"type":"response.completed","response":{"id":"resp-call","status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":4}}})),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::Responses, url)).unwrap();
    let response = model.respond_with_metadata(request()).await.unwrap();
    assert_eq!(
        response.response.finish_reason,
        ModelFinishReason::ToolCalls
    );
    assert_eq!(response.response.calls()[0].id, "call1");
    assert_eq!(response.provider_state.unwrap().payload, output);
    task.await.unwrap();
}

#[tokio::test]
async fn messages_tool_json_settles_canonically_and_replays_error_results() {
    let (url, task) = serve("200 OK", "text/event-stream", vec![
        sse(json!({"type":"message_start","message":{"id":"msg-tool","role":"assistant","content":[],"usage":{"input_tokens":4,"output_tokens":0}}})),
        sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool1","name":"lookup","input":{}}})),
        sse(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{ \"x\":"}})),
        sse(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":" 1 }"}})),
        sse(json!({"type":"content_block_stop","index":0})),
        sse(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}})),
        sse(json!({"type":"message_stop"})),
    ]).await;
    let model = HttpModel::new(config(ProviderProtocol::AnthropicMessages, url)).unwrap();
    let output = model.respond_with_metadata(request()).await.unwrap();
    assert_eq!(output.response.calls()[0].arguments, "{\"x\":1}");
    let mut next = request();
    next.messages.push(ModelMessage::assistant_blocks(
        output.response.content,
        output.provider_state,
    ));
    next.messages.push(ModelMessage::tool_result(
        "tool1".into(),
        "Not found".into(),
        true,
    ));
    let body = model.encode(&next).unwrap();
    assert_eq!(body["messages"][2]["content"][0]["is_error"], true);
    task.await.unwrap();
}

fn screenshot_result(call_id: &str) -> ModelMessage {
    let mut message =
        ModelMessage::tool_result(call_id.into(), "Screenshot captured".into(), false);
    if let ContentBlock::ToolResult { images, .. } = &mut message.content[0] {
        images.push(agent_core::ModelImage::new("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZcZkAAAAASUVORK5CYII=").unwrap());
    }
    message
}

#[test]
fn multimodal_tool_results_use_each_protocols_image_contract() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        let mut cfg = config(protocol, "http://127.0.0.1:12345/v1".into());
        cfg.supports_images = true;
        let model = HttpModel::new(cfg).unwrap();
        let mut req = request();
        req.messages.push(screenshot_result("capture-1"));
        req.messages.push(ModelMessage::tool_result(
            "capture-2".into(),
            "other tool".into(),
            false,
        ));
        let encoded = model.encode(&req).unwrap();
        match protocol {
            ProviderProtocol::ChatCompletions => {
                let messages = encoded["messages"].as_array().unwrap();
                assert_eq!(messages[2]["role"], "tool");
                assert_eq!(messages[3]["role"], "tool");
                assert_eq!(messages[4]["role"], "user");
                assert!(
                    messages[4]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("capture-1")
                );
                assert_eq!(messages[4]["content"][1]["type"], "image_url");
                assert!(
                    messages[4]["content"][1]["image_url"]["url"]
                        .as_str()
                        .unwrap()
                        .starts_with("data:image/png;base64,")
                );
                assert!(!messages[2]["content"].as_str().unwrap().contains("base64"));
            }
            ProviderProtocol::Responses => {
                assert_eq!(encoded["input"][1]["type"], "function_call_output");
                assert_eq!(encoded["input"][1]["output"][1]["type"], "input_image");
                assert!(
                    encoded["input"][1]["output"][1]["image_url"]
                        .as_str()
                        .unwrap()
                        .starts_with("data:image/png;base64,")
                );
            }
            ProviderProtocol::AnthropicMessages => {
                let output = &encoded["messages"][1]["content"][0];
                assert_eq!(output["type"], "tool_result");
                assert_eq!(output["content"][1]["type"], "image");
                assert_eq!(output["content"][1]["source"]["type"], "base64");
                assert_eq!(output["content"][1]["source"]["media_type"], "image/png");
            }
        }
    }
}

#[test]
fn image_input_requires_explicit_model_support_and_valid_image_data() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        let mut req = request();
        req.messages.push(screenshot_result("capture"));
        assert_eq!(
            model(protocol).encode(&req).unwrap_err(),
            ProviderError::ImagesUnsupported
        );
        if let ContentBlock::ToolResult { images, .. } = &mut req.messages[1].content[0] {
            images[0].data_base64 = "not an image".into();
        }
        assert_eq!(
            model(protocol).encode(&req).unwrap_err(),
            ProviderError::InvalidRequest
        );
    }
}

#[tokio::test]
async fn screenshots_are_sent_as_structured_images_over_http_for_all_three_protocols() {
    for (protocol, response) in [
        (
            ProviderProtocol::ChatCompletions,
            json!({"id":"c1","choices":[{"message":{"role":"assistant","content":"observed"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":1}}),
        ),
        (
            ProviderProtocol::Responses,
            json!({"id":"r1","status":"completed","output":[{"id":"m1","type":"message","role":"assistant","content":[{"type":"output_text","text":"observed"}]}],"usage":{"input_tokens":20,"output_tokens":1}}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"observed"}],"stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":1}}),
        ),
    ] {
        let (url, server) = serve("200 OK", "application/json", vec![response.to_string()]).await;
        let mut cfg = config(protocol, url);
        cfg.supports_images = true;
        let model = HttpModel::new(cfg).unwrap();
        let mut req = request();
        req.messages.push(screenshot_result("capture"));
        assert_eq!(model.respond(req).await.unwrap().text(), "observed");
        let sent = server.await.unwrap();
        let body: Value = serde_json::from_str(sent.split_once("\r\n\r\n").unwrap().1).unwrap();
        let image_type = match protocol {
            ProviderProtocol::ChatCompletions => &body["messages"][3]["content"][1]["type"],
            ProviderProtocol::Responses => &body["input"][1]["output"][1]["type"],
            ProviderProtocol::AnthropicMessages => {
                &body["messages"][1]["content"][0]["content"][1]["type"]
            }
        };
        assert!(matches!(
            image_type.as_str(),
            Some("image_url" | "input_image" | "image")
        ));
    }
}
