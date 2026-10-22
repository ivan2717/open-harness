# agent-mcp

An MCP client boundary built on the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), pinned to `rmcp 3.6.0`. The SDK handles protocol types, initialization negotiation, JSON-RPC correlation, cancellation and Streamable HTTP sessions. Open Harness adds host configuration, bounded messages and process cleanup. The design follows Codex's separation between MCP transport clients and the host's tool approval/execution layer.

## Supported operations

- stdio: direct executable plus argument vector; no implicit shell or package installation.
- Streamable HTTP: HTTPS, or loopback HTTP for local servers; JSON and SSE responses.
- `initialize` / `notifications/initialized`, requesting MCP `2025-11-25` and accepting SDK-supported legacy negotiation.
- Paginated tools, resources, resource templates and prompts discovery.
- Tool calls, resource reads and prompt retrieval. Tool call output retains MCP `content`, `structuredContent`, `isError` and metadata, including non-text content blocks.
- Bearer tokens supplied by environment variable name. Values are resolved immediately before connecting and never serialized by the configuration.

Configuration stores only environment references. Put credentials in environment variables; do not embed credentials in command arguments or URL paths. HTTP URLs reject credentials, query strings and fragments, and redirects are disabled. An HTTP endpoint may still return untrusted data. A stdio server is a locally executed application, not a project-sandboxed built-in tool: the host must communicate and authorize that distinction.

## Lifecycle and limits

`McpClient::new` validates configuration without connecting. The first operation opens a session and completes the handshake. Clones of the client share this session, and operations are serialized, so stateful tools such as browser navigation and screenshots retain server state between invocations. The host caches clients by configuration and invalidates them when configuration changes. `shutdown` explicitly closes the shared session; dropping the final client also closes it. Transport failures, malformed responses and cancelled operations invalidate the affected session; correlated tool RPC errors keep the healthy session, so correcting an invalid argument does not discard state. A later explicit operation may establish a new session; no failed invocation is replayed.

For cache eviction, `is_idle` requires both a free session lock and no outstanding client clones, including handles that have not yet started their operation. Configuration changes use `revoke` to permanently invalidate all clones and cancel their underlying SDK sessions. A revoked handle cannot reconnect; the host must obtain a client for the current configuration.

The default deadline is 60 seconds for the entire operation, including waiting for the session lock, handshake and pagination; hosts may set 10 ms–300 seconds using `with_timeout`. Cleanup has a separate two-second deadline. Each operation takes ownership of the active session until it completes, so dropping an in-flight future cancels the SDK service instead of leaving it cached. Stdio uses kill-on-drop, closes pipes, kills its dedicated process group on Unix and reaps its child. Non-Unix cleanup guarantees the direct child only. HTTP sends session DELETE when supported; cancellation cannot undo an operation already performed by a remote server.

Messages and complete HTTP response streams are capped at 4 MiB before parsing. This includes unterminated stdio lines and SSE events. Each list has a 4 MiB aggregate cap, 512 items and 32 pages; repeated cursors and duplicate identities fail. Input payloads are bounded too. Transparent expired-session recovery is disabled, so a side-effecting POST is never automatically replayed. SSE GET reconnection is handled by the SDK within the operation deadline.

Child environment is cleared, then populated with a small platform baseline (`PATH`, home/system/temp/locale variables) and explicitly requested environment variable names. Missing requested variables are errors. Child stderr is discarded; public transport errors omit server payloads, URLs, arguments, and environment values. Call results remain untrusted server output and may themselves contain sensitive material supplied by that server.

## Host responsibilities and current exclusions

The host authorizes saving/enabling servers, launching processes, contacting remote servers, and invoking tools. Tool annotations such as `readOnlyHint` are advisory and never grant permission. Server instructions and prompt text are untrusted content; this crate does not inject them into system instructions. Saving a configuration does not connect or install software.

OAuth, roots, sampling, elicitation, subscriptions, asynchronous MCP tasks, 2026 discovery/MRTR, and the old two-endpoint HTTP+SSE transport are not implemented or advertised. Servers requiring these features should report a compatibility error. No OAuth browser flow or automatic credential fallback is attempted.

## Validation

`cargo test -p agent-mcp` runs local stdio and loopback HTTP fixtures, requiring Python 3 for the stdio test peer and permission to bind loopback sockets. It checks initialization, pagination, capabilities, JSON/SSE output, resources/prompts, `isError`, structured content, sanitized errors, bounds, deadlines, shared-session state, cancellation/reaping, last-client cleanup, negotiated session headers and no replay after session expiration. No external server or model API is contacted.
