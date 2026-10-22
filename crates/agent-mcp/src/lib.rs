//! Bounded MCP clients for explicitly configured local processes and HTTP servers.
//! Configuration and discovery never imply permission to invoke a tool. Hosts must
//! approve process startup/remote access and tool calls before using these APIs.
mod config;
mod http;
#[cfg(test)]
mod lifecycle_tests;
mod stdio;

use std::{collections::HashSet, future::Future, sync::Arc, time::Duration};

pub use config::{McpServerConfig, McpTransport};
use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, ClientConfig, GetPromptRequestParams, Implementation,
        PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
    },
    service::{RoleClient, RunningService, ServiceError},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CATALOG_ITEMS: usize = 512;
const MAX_CATALOG_PAGES: usize = 32;
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
type Session = RunningService<RoleClient, ClientConfig>;
pub type Result<T> = std::result::Result<T, McpError>;

/// Error messages intentionally omit server response bodies, URLs, arguments,
/// environment values and stderr, any of which can contain credentials.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("invalid MCP configuration: {0}")]
    Config(&'static str),
    #[error("MCP server is disabled")]
    Disabled,
    #[error("MCP server configuration was revoked; reload the current server configuration")]
    Revoked,
    #[error("a configured MCP environment variable is missing or invalid")]
    Environment,
    #[error("could not launch the MCP process; check its executable and arguments")]
    Launch,
    #[error("MCP connection failed; check the server, transport and authentication configuration")]
    Connect,
    #[error("MCP operation timed out; its effects may already have occurred")]
    Timeout,
    #[error("MCP request failed; check that the server supports this operation")]
    Request,
    #[error("MCP server rejected the request; check its name and arguments")]
    Rejected,
    #[error("MCP response or catalog exceeds the supported size limit")]
    Limit,
    #[error("MCP server returned a repeated pagination cursor or duplicate item")]
    Pagination,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    /// Advisory server metadata; never treat readOnlyHint as authorization.
    pub annotations: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpCatalog {
    pub protocol_version: String,
    pub server_info: Value,
    pub capabilities: Value,
    pub tools: Vec<McpTool>,
    pub resources: Vec<Value>,
    pub resource_templates: Vec<Value>,
    pub prompts: Vec<Value>,
}

/// Clones share a negotiated session so stateful tools can span invocations.
/// Calls are serialized. Dropping an in-flight operation invalidates that session
/// and cancels the SDK service and its transport, without replaying the call.
#[derive(Clone, Debug)]
pub struct McpClient {
    config: McpServerConfig,
    timeout: Duration,
    session: Arc<tokio::sync::Mutex<Option<Session>>>,
    revocation: tokio_util::sync::CancellationToken,
}

impl McpClient {
    pub fn new(config: McpServerConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            timeout: Duration::from_secs(60),
            session: Arc::new(tokio::sync::Mutex::new(None)),
            revocation: tokio_util::sync::CancellationToken::new(),
        })
    }

    /// An operation's whole deadline includes initialization and all list pages.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.clamp(Duration::from_millis(10), Duration::from_secs(300));
        self
    }

    /// Whether a host cache can evict this client without disrupting a caller.
    /// An outstanding clone may be about to start an operation, even when the
    /// session lock is free, so such a client is conservatively considered busy.
    pub fn is_idle(&self) -> bool {
        Arc::strong_count(&self.session) == 1 && self.session.try_lock().is_ok()
    }

    /// Permanently invalidate every clone after host configuration changes.
    /// In-flight operations are cancelled and cannot repopulate the cache.
    /// As with any cancellation, remote effects that already happened remain.
    pub fn revoke(&self) {
        self.revocation.cancel();
        if let Ok(mut session) = self.session.try_lock() {
            session.take();
        }
    }

    async fn connect(&self) -> Result<Session> {
        if !self.config.enabled {
            return Err(McpError::Disabled);
        }
        let mut client = ClientConfig::new(
            Default::default(),
            Implementation::new("open-harness", env!("CARGO_PKG_VERSION")),
        );
        // Use initialize lifecycle; 2026 discovery/MRTR are not advertised.
        client.protocol_version = ProtocolVersion::V_2025_11_25;
        match &self.config.transport {
            McpTransport::Stdio {
                command,
                args,
                env_vars,
            } => client
                .serve_with_ct(
                    stdio::StdioTransport::spawn(command, args, env_vars)?,
                    self.revocation.child_token(),
                )
                .await
                .map_err(|_| McpError::Connect),
            McpTransport::Http {
                url,
                bearer_token_env,
            } => {
                let http = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .connect_timeout(Duration::from_secs(10))
                    .build()
                    .map_err(|_| McpError::Connect)?;
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                    .max_sse_event_size(MAX_MESSAGE_BYTES)
                    .reinit_on_expired_session(false)
                    .max_concurrent_requests(1)
                    .control_request_timeout(SHUTDOWN_TIMEOUT);
                if let Some(name) = bearer_token_env {
                    let token = std::env::var(name).map_err(|_| McpError::Environment)?;
                    if token.is_empty() || token.contains(['\r', '\n']) {
                        return Err(McpError::Environment);
                    }
                    config = config.auth_header(token);
                }
                client
                    .serve_with_ct(
                        StreamableHttpClientTransport::with_client(
                            http::BoundedHttpClient(http),
                            config,
                        ),
                        self.revocation.child_token(),
                    )
                    .await
                    .map_err(|_| McpError::Connect)
            }
        }
    }

    async fn operation<T>(
        &self,
        f: impl for<'a> FnOnce(
            &'a Session,
        )
            -> std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
    ) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.revocation.cancelled() => Err(McpError::Revoked),
            result = self.operation_inner(f) => result,
        }
    }

    async fn operation_inner<T>(
        &self,
        f: impl for<'a> FnOnce(
            &'a Session,
        )
            -> std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
    ) -> Result<T> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut cached = tokio::time::timeout_at(deadline, self.session.lock())
            .await
            .map_err(|_| McpError::Timeout)?;
        // Own the session outside the cache until the operation completes. If the
        // future is aborted, Drop closes it and no caller can reuse uncertain state.
        let mut session = match cached.take() {
            Some(session) if !session.is_closed() => session,
            _ => tokio::time::timeout_at(deadline, self.connect())
                .await
                .map_err(|_| McpError::Timeout)??,
        };
        let result = tokio::time::timeout_at(deadline, f(&session))
            .await
            .map_err(|_| McpError::Timeout)
            .and_then(|result| result);
        if (result.is_ok() || matches!(&result, Err(McpError::Rejected)))
            && !session.is_closed()
            && !self.revocation.is_cancelled()
        {
            *cached = Some(session);
        } else {
            let _ = session.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        result
    }

    /// Close the shared session. A later operation may explicitly reconnect.
    pub async fn shutdown(&self) {
        if let Some(mut session) = self.session.lock().await.take() {
            let _ = session.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
    }

    pub async fn test_connection(&self) -> Result<Value> {
        self.operation(|session| Box::pin(async move { json(session.peer_info().as_deref()) }))
            .await
    }

    pub async fn discover(&self) -> Result<McpCatalog> {
        self.operation(|session| {
            Box::pin(async move {
                let info = session.peer_info().ok_or(McpError::Connect)?;
                let mut tools = Vec::new();
                if info.capabilities.tools.is_some() {
                    for value in list(session, CatalogKind::Tools).await? {
                        let tool: rmcp::model::Tool =
                            serde_json::from_value(value).map_err(|_| McpError::Request)?;
                        tools.push(McpTool {
                            name: tool.name.into_owned(),
                            description: tool
                                .description
                                .map(|description| description.into_owned()),
                            input_schema: json(&*tool.input_schema)?,
                            annotations: tool
                                .annotations
                                .map(|annotations| json(&annotations))
                                .transpose()?,
                        });
                    }
                }
                let resources = if info.capabilities.resources.is_some() {
                    list(session, CatalogKind::Resources).await?
                } else {
                    Vec::new()
                };
                let resource_templates = if info.capabilities.resources.is_some() {
                    list(session, CatalogKind::ResourceTemplates).await?
                } else {
                    Vec::new()
                };
                let prompts = if info.capabilities.prompts.is_some() {
                    list(session, CatalogKind::Prompts).await?
                } else {
                    Vec::new()
                };
                Ok(McpCatalog {
                    protocol_version: info.protocol_version.to_string(),
                    server_info: json(&info.server_info)?,
                    capabilities: json(&info.capabilities)?,
                    tools,
                    resources,
                    resource_templates,
                    prompts,
                })
            })
        })
        .await
    }

    /// Preserve MCP `content`, `structuredContent`, `isError` and metadata.
    /// Tool-level errors are results, not transport failures; never auto-retry.
    pub async fn call_tool(&self, name: &str, arguments: Map<String, Value>) -> Result<Value> {
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        check_size(&params)?;
        self.operation(move |session| {
            Box::pin(async move {
                json(
                    &session
                        .peer()
                        .call_tool(params)
                        .await
                        .map_err(request_error)?,
                )
            })
        })
        .await
    }

    pub async fn list_resources(&self) -> Result<Vec<Value>> {
        self.operation(|session| Box::pin(list(session, CatalogKind::Resources)))
            .await
    }

    pub async fn list_resource_templates(&self) -> Result<Vec<Value>> {
        self.operation(|session| Box::pin(list(session, CatalogKind::ResourceTemplates)))
            .await
    }

    pub async fn read_resource(&self, uri: &str) -> Result<Value> {
        let params = ReadResourceRequestParams::new(uri);
        check_size(&params)?;
        self.operation(move |session| {
            Box::pin(async move {
                json(
                    &session
                        .peer()
                        .read_resource(params)
                        .await
                        .map_err(request_error)?,
                )
            })
        })
        .await
    }

    pub async fn list_prompts(&self) -> Result<Vec<Value>> {
        self.operation(|session| Box::pin(list(session, CatalogKind::Prompts)))
            .await
    }

    pub async fn get_prompt(&self, name: &str, arguments: Map<String, Value>) -> Result<Value> {
        let params = GetPromptRequestParams::new(name).with_arguments(arguments);
        check_size(&params)?;
        self.operation(move |session| {
            Box::pin(async move {
                json(
                    &session
                        .peer()
                        .get_prompt(params)
                        .await
                        .map_err(request_error)?,
                )
            })
        })
        .await
    }
}

fn check_size(value: &impl Serialize) -> Result<()> {
    if serde_json::to_vec(value)
        .map_err(|_| McpError::Request)?
        .len()
        > MAX_MESSAGE_BYTES
    {
        Err(McpError::Limit)
    } else {
        Ok(())
    }
}

fn request_error(error: ServiceError) -> McpError {
    match error {
        // A correlated JSON-RPC error leaves the session usable. In particular,
        // correcting an invalid tool argument must not reset browser state.
        ServiceError::McpError(_) => McpError::Rejected,
        _ => McpError::Request,
    }
}

fn json(value: impl Serialize) -> Result<Value> {
    check_size(&value)?;
    serde_json::to_value(value).map_err(|_| McpError::Request)
}

enum CatalogKind {
    Tools,
    Resources,
    ResourceTemplates,
    Prompts,
}

async fn list(session: &Session, kind: CatalogKind) -> Result<Vec<Value>> {
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut names = HashSet::new();
    let mut items = Vec::new();
    let mut bytes = 0usize;
    for _ in 0..MAX_CATALOG_PAGES {
        let mut params = PaginatedRequestParams::default();
        params.cursor = cursor;
        let params = Some(params);
        let page = match kind {
            CatalogKind::Tools => json(
                session
                    .list_tools(params)
                    .await
                    .map_err(|_| McpError::Request)?,
            )?,
            CatalogKind::Resources => json(
                session
                    .list_resources(params)
                    .await
                    .map_err(|_| McpError::Request)?,
            )?,
            CatalogKind::ResourceTemplates => json(
                session
                    .list_resource_templates(params)
                    .await
                    .map_err(|_| McpError::Request)?,
            )?,
            CatalogKind::Prompts => json(
                session
                    .list_prompts(params)
                    .await
                    .map_err(|_| McpError::Request)?,
            )?,
        };
        bytes = bytes.saturating_add(
            serde_json::to_vec(&page)
                .map_err(|_| McpError::Request)?
                .len(),
        );
        if bytes > MAX_CATALOG_BYTES {
            return Err(McpError::Limit);
        }
        let key = match kind {
            CatalogKind::Tools => "tools",
            CatalogKind::Resources => "resources",
            CatalogKind::ResourceTemplates => "resourceTemplates",
            CatalogKind::Prompts => "prompts",
        };
        for item in page
            .get(key)
            .and_then(Value::as_array)
            .ok_or(McpError::Request)?
        {
            if items.len() >= MAX_CATALOG_ITEMS {
                return Err(McpError::Limit);
            }
            let identity_key = match kind {
                CatalogKind::Resources => "uri",
                CatalogKind::ResourceTemplates => "uriTemplate",
                _ => "name",
            };
            let identity = item
                .get(identity_key)
                .and_then(Value::as_str)
                .ok_or(McpError::Request)?;
            if !names.insert(identity.to_owned()) {
                return Err(McpError::Pagination);
            }
            items.push(item.clone());
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match &cursor {
            None => return Ok(items),
            Some(cursor) if !cursors.insert(cursor.clone()) => return Err(McpError::Pagination),
            _ => {}
        }
    }
    Err(McpError::Limit)
}
