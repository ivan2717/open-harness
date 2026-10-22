//! SDK lifecycle and SSE parsing, with bounded HTTP bodies and no redirects.
use std::{collections::HashMap, io, sync::Arc};

use futures::{StreamExt, TryStreamExt, stream::BoxStream};
use http::{HeaderName, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::streamable_http_client::{
        StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::{Error as SseError, Sse, SseStream};

use crate::MAX_MESSAGE_BYTES;

type Error = StreamableHttpError<io::Error>;

#[derive(Clone)]
pub(crate) struct BoundedHttpClient(pub Client);

fn failure(message: &'static str) -> Error {
    Error::Client(io::Error::other(message))
}

impl BoundedHttpClient {
    fn request(
        &self,
        method: Method,
        uri: &str,
        session: Option<&str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> RequestBuilder {
        let mut request = self
            .0
            .request(method, uri)
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(auth) = auth {
            request = request.bearer_auth(auth);
        }
        request
    }
}

fn sse(response: reqwest::Response) -> BoxStream<'static, std::result::Result<Sse, SseError>> {
    // Bound each HTTP response stream, including unterminated events, before the
    // SSE parser allocates its buffer. The SDK may reconnect the notification GET.
    let bytes = response.bytes_stream().scan(0usize, |total, chunk| {
        let result = chunk
            .map_err(|_| io::Error::other("MCP response stream failed"))
            .and_then(|chunk| {
                *total = total.saturating_add(chunk.len());
                if *total > MAX_MESSAGE_BYTES {
                    Err(io::Error::other("MCP response exceeds the size limit"))
                } else {
                    Ok(chunk)
                }
            });
        futures::future::ready(Some(result))
    });
    SseStream::from_bytes_stream(bytes).boxed()
}

async fn body(response: reqwest::Response) -> std::result::Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MESSAGE_BYTES as u64)
    {
        return Err(failure("MCP response exceeds the size limit"));
    }
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|_| failure("MCP response stream failed"))?
    {
        if output.len().saturating_add(chunk.len()) > MAX_MESSAGE_BYTES {
            return Err(failure("MCP response exceeds the size limit"));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

impl StreamableHttpClient for BoundedHttpClient {
    type Error = io::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<StreamableHttpPostResponse, Error> {
        let response = self
            .request(
                Method::POST,
                &uri,
                session_id.as_deref(),
                auth_header,
                custom_headers,
            )
            .json(&message)
            .send()
            .await
            .map_err(|_| failure("MCP HTTP request failed"))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND && session_id.is_some() {
            return Err(Error::SessionExpired);
        }
        if !status.is_success() {
            return Err(failure(
                if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    "MCP authorization failed; configure bearer_token_env (OAuth is not supported)"
                } else {
                    "MCP server rejected the HTTP request"
                },
            ));
        }
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let is_request = matches!(message, ClientJsonRpcMessage::Request(_));
        if response.content_length() == Some(0) && !is_request {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if content_type.starts_with("text/event-stream") {
            return Ok(StreamableHttpPostResponse::Sse(sse(response), session));
        }
        if content_type.starts_with("application/json") {
            let bytes = body(response).await?;
            return match serde_json::from_slice(&bytes) {
                Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session)),
                Err(_) if !is_request => Ok(StreamableHttpPostResponse::Accepted),
                Err(_) => Err(failure("MCP server returned invalid JSON-RPC")),
            };
        }
        Err(failure("MCP server returned an unsupported content type"))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<(), Error> {
        let response = self
            .request(
                Method::DELETE,
                &uri,
                Some(&session_id),
                auth_header,
                custom_headers,
            )
            .send()
            .await
            .map_err(|_| failure("MCP session cleanup failed"))?;
        if response.status().is_success()
            || response.status() == StatusCode::METHOD_NOT_ALLOWED
            || response.status() == StatusCode::NOT_FOUND
        {
            Ok(())
        } else {
            Err(failure("MCP session cleanup was rejected"))
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<BoxStream<'static, std::result::Result<Sse, SseError>>, Error> {
        let mut request = self.request(
            Method::GET,
            &uri,
            session_id.as_deref(),
            auth_header,
            custom_headers,
        );
        if let Some(last_event_id) = last_event_id {
            request = request.header("last-event-id", last_event_id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| failure("MCP event stream failed"))?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(Error::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(failure("MCP event stream was rejected"));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err(failure("MCP event stream has invalid content type"));
        }
        Ok(sse(response))
    }
}
