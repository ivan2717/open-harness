//! One-shot wire diagnostics for synthetic boundary fixtures only.
//! Does not alter production parsing or preserve request / response bodies.
use super::fixture::{OUTPUT_RESERVE, fixture_request};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    time::{Duration, Instant},
};

pub async fn run(
    provider: &str,
    model: &str,
    rows: usize,
    key_env: &str,
) -> Result<Value, Box<dyn Error>> {
    if rows > 300_000 {
        return Err("diagnostic row bound exceeded".into());
    }
    let key = std::env::var(key_env).map_err(|_| "missing diagnostic credential")?;
    let request = fixture_request(rows);
    let (base, endpoint, body) = if provider == "openai" {
        (
            "https://api.openai.com/v1",
            "responses",
            json!({
                "model":model, "instructions":request.instructions,
                "input":[{"role":"user","content":[{"type":"input_text","text":request.messages[0].text()}]}],
                "stream":true,"store":false,"include":["reasoning.encrypted_content"],
                "max_output_tokens":OUTPUT_RESERVE,"reasoning":{"effort":"high"}
            }),
        )
    } else {
        (
            "https://api.deepseek.com",
            "chat/completions",
            json!({
                "model":model,"messages":[{"role":"system","content":request.instructions},{"role":"user","content":request.messages[0].text()}],
                "stream":true,"stream_options":{"include_usage":true},"max_tokens":OUTPUT_RESERVE,
                "thinking":{"type":"enabled"},"reasoning_effort":"high"
            }),
        )
    };
    let base = std::env::var("API_BASE_URL").unwrap_or_else(|_| base.into());
    let url = reqwest::Url::parse(&format!("{}/{endpoint}", base.trim_end_matches('/')))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("diagnostic requires an HTTPS endpoint without URL credentials".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| "invalid diagnostic credential")?;
    auth.set_sensitive(true);
    let started = Instant::now();
    let mut response = client
        .post(url)
        .header(AUTHORIZATION, auth)
        .json(&body)
        .send()
        .await
        .map_err(|_| "diagnostic transport failed")?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let is_sse = content_type.contains("text/event-stream");
    let mut result = json!({"http_status":status,"content_type":content_type,"request_count":1,"retries":0,"scope":"synthetic fixture wire diagnostic only; no request or response bodies retained"});
    let mut pending = Vec::<u8>::new();
    let mut data = Vec::<u8>::new();
    let mut types = BTreeMap::<String, usize>::new();
    let mut bytes = 0usize;
    let mut events = 0usize;
    let mut parse_errors = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "diagnostic response read failed")?
    {
        bytes += chunk.len();
        if bytes > 32 * 1024 * 1024 {
            return Err("diagnostic response exceeds 32 MiB".into());
        }
        pending.extend_from_slice(&chunk);
        if !is_sse {
            continue;
        }
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let mut line = pending.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() && !data.is_empty() {
                events += 1;
                if data.starts_with(b"[DONE]") {
                    *types.entry("[DONE]".into()).or_default() += 1;
                } else if let Ok(event) = serde_json::from_slice::<Value>(&data) {
                    let kind = event["type"]
                        .as_str()
                        .or_else(|| event["object"].as_str())
                        .unwrap_or("untyped");
                    *types.entry(safe_identifier(kind)).or_default() += 1;
                    summarize(&event, &key, &mut result);
                } else {
                    parse_errors += 1;
                }
                data.clear();
            } else if let Some(part) = line.strip_prefix(b"data:") {
                data.extend_from_slice(part.strip_prefix(b" ").unwrap_or(part));
                data.push(b'\n');
            }
            if data.len() > 4 * 1024 * 1024 {
                return Err("diagnostic event exceeds 4 MiB".into());
            }
        }
        if pending.len() > 4 * 1024 * 1024 {
            return Err("diagnostic line exceeds 4 MiB".into());
        }
    }
    if !is_sse {
        if let Ok(event) = serde_json::from_slice::<Value>(&pending) {
            summarize(&event, &key, &mut result);
        } else {
            parse_errors += 1;
        }
    }
    result["event_types"] = json!(types);
    result["sse_events"] = json!(events);
    result["json_parse_errors"] = json!(parse_errors);
    result["response_bytes"] = json!(bytes);
    result["elapsed_ms"] = json!(started.elapsed().as_millis());
    Ok(result)
}

fn safe_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '[' | ']'))
        .take(128)
        .collect()
}

fn summarize(event: &Value, key: &str, result: &mut Value) {
    let response = event.get("response").unwrap_or(event);
    if let Some(status) = response["status"].as_str() {
        result["final_status"] = json!(safe_identifier(status));
    }
    if let Some(usage) = response
        .get("usage")
        .or_else(|| event.get("usage"))
        .filter(|u| u.is_object())
    {
        let mut counts = json!({});
        for field in [
            "input_tokens",
            "output_tokens",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
        ] {
            if let Some(value) = usage[field].as_u64() {
                counts[field] = json!(value);
            }
        }
        result["usage"] = counts;
    }
    let error = event
        .get("error")
        .filter(|v| !v.is_null())
        .or_else(|| response.get("error").filter(|v| !v.is_null()))
        .or_else(|| (event["type"] == "error").then_some(event));
    if let Some(error) = error {
        let message = error["message"]
            .as_str()
            .unwrap_or("")
            .replace(key, "[REDACTED]");
        let message = message
            .split_whitespace()
            .map(|word| {
                if word.contains("sk-") {
                    "[REDACTED]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        result["provider_error"] = json!({
            "code": error["code"].as_str().map(safe_identifier),
            "type": error["type"].as_str().map(safe_identifier),
            "message": message.chars().take(512).collect::<String>()
        });
    }
    if let Some(reason) = response["incomplete_details"]["reason"].as_str() {
        result["incomplete_reason"] = json!(safe_identifier(reason));
    }
}
