# Open Harness

A Rust workspace for an event-driven agent harness, with an Electron desktop
workbench. The runtime coordinates models, tools, bounded model context, durable
event storage, and cancellation. The desktop connects to a local Rust API to
show conversation output, tool execution, context budgets, and compaction.

## Desktop

The desktop uses **Electron + React + TypeScript + Zustand + Tailwind CSS +
TanStack Query + TanStack Router**. With Node.js 24 LTS and Rust installed:

```sh
cd apps/desktop
npm ci
npm run dev
```

The desktop includes an offline demonstration model and configurable real-model
connections through Chat Completions, Responses, and Anthropic Messages. Settings
accept the model's capacity and reasoning configuration; credentials are encrypted
by Electron's OS secret facility when available. Conversations support follow-up
turns and cancellable manual context compaction. The offline demo makes no model
API requests. See [desktop setup and architecture](apps/desktop/README.md)
for state ownership, persistence, checks, and packaging.

### Local project tasks

Choose a project directory and a configured model to work on actual local files.
New projects default to **read-only**. With **workspace write** enabled, the model
can make focused edits; each shell command still requires a separate approval.
The desktop shows pending approvals, command results, and file changes in an
on-demand project panel. The offline demonstration does not operate on projects.

The model sees up to five local tools: `list_files`, `read_file`, `search_files`,
`apply_patch`, and `run_command`. Read-only projects expose just the first three.
Reading and searching automatically handle UTF-8 text and PDF, DOCX, XLSX and
PPTX documents, preserving page/paragraph/slide/cell locations and pagination.
Directory searches scan bounded text files and report skipped documents for
individual reads/searches. Patches create, update, or delete files; context
matches must be unique, and all changes are validated before the first write.
Scanned documents require OCR, which is not yet included. Paths
are relative to the selected directory; symlinks, hard-linked files, credential
paths, and Git internals are rejected by file tools. Project `AGENTS.md` guidance
is captured when the conversation starts; nested guidance can be read on demand.
Each conversation keeps its original instruction snapshot when it is reopened,
so persisted provider reasoning and prefix bindings remain valid.

Command execution currently requires the macOS `sandbox-exec` facility. Commands
run with a clean environment, network access disabled, project-scoped writes,
bounded output, a 60-second default timeout (120 seconds maximum), and owned
process-group cancellation. A pipe watchdog also stops the owned process group
when the host exits unexpectedly. Commands must be foreground tasks: daemons or
processes that deliberately detach into a new session are unsupported and cannot
be guaranteed to stop with that group. If the supported sandbox is unavailable, commands
are refused; there is no unrestricted fallback. Other platforms can still use
file tools. Projects need installed dependencies for offline test commands.

File-tool changes are journaled before writing and can be reverted while idle
only if the file still matches the agent's last saved content. This preserves
pre-existing user modifications and refuses to overwrite later external edits.
Command changes are recorded for review but require manual resolution, because
the host cannot reliably distinguish command writes from concurrent user edits.
Review snapshots exclude dependency/build directories and are bounded; they are
not a full filesystem backup or a Git reset. Overlapping project directories
cannot execute in two sessions concurrently within the application service.

Interrupted, cancelled, failed, and completed conversations can continue in place.
Recovery commits an explicit event, keeps completed results, and marks missing
tool outcomes as unknown or not started. It never re-executes old tool calls.
The model is instructed to inspect actual state before taking further action.
Missing project directories disable execution without making history unreadable.

### Browser and computer operation

Open **插件** in the desktop rail to install the optional bundled **Browser Use**
and **Computer Use** plugins. Each bundles a Skill and a local MCP executor;
the existing harness runs the model/tool loop, approvals, history and cancellation.
Installation does not start a browser, operate an app, or call a model.

**Browser Use** drives an isolated Playwright Chromium session through bounded
page text, accessibility observations, element references and screenshots. Click
**准备浏览器** once to download the matching Chromium, then install the plugin.
This version supports macOS and Linux; Windows has no supported launcher yet.
It does not attach to the user's existing browser profile. See the
[Browser Use contract and limitations](plugins/browser-use/README.md).

**Computer Use** operates one explicitly selected running macOS application.
It requires macOS 14 or later, Accessibility permission, and Screen Recording
permission for window screenshots. The plugin card offers **辅助功能**,
**屏幕录制** and **检查权限**; permission prompts only follow those user actions.
Building from source also requires Xcode Command Line Tools. There is no native
Windows/Linux desktop executor in this version. See the
[Computer Use setup and GUI checks](plugins/computer-use/README.md).

Before a first action, the conversation offers **允许本次** or a scoped
**允许本会话** grant. Browser grants apply to its isolated session; computer grants
bind one application/process, with another approval required to switch apps.
Use **停止控制** to revoke the grant and stop its owned executor; run cancellation
and application shutdown also clean up owned work. Stopping cannot undo an input
already delivered. Screenshots and UI text are observations, not authorization.

In **模型 → 接入模型**, enable **此模型支持图片输入** only for a vision-capable
model. Structured UI observations remain available without this option, while
omitted screenshots are explicitly reported. Chat Completions, Responses and
Messages adapt real image blocks separately. The runtime retains only the latest
screenshot group in active context and memory; older images remain in checked
SHA256 sidecars for explicit archive reads. Live notifications carry references
so slow subscribers do not accumulate screenshot pixels. Context budgets reserve an image
allowance without counting base64 as text. Images never enter Wiki or compaction
as imagined descriptions. The long-run tests cover 40 successive screenshots,
restore/continue, complete archive retrieval and corrupt-image rejection on read.

After preparing Chromium, validate from the repository root with
`npm --prefix apps/desktop run test:browser`,
`npm --prefix apps/desktop run test:computer`, and
`npm --prefix apps/desktop run test:e2e:computer`. The native macOS fixture uses
`node plugins/computer-use/tests/gui-smoke.mjs`; missing OS permissions report
`blocked`, not a successful GUI run. Rust protocol and retention tests run with
`cargo test -p agent-core -p agent-runtime -p agent-memory -p agent-providers --offline`.

### Plugins, MCP and Skills

The built-in **LLM Wiki** memory plugin is enabled by default and can be turned
off on the Plugins page. Each conversation has its own wiki, organized into a
short topic guide and linked pages with event citations. The run inspector's
**Memory / 记忆** tab shows these notes and links back to their timeline sources.
This follows [Karpathy's LLM Wiki pattern](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f):
retain original sources, then maintain an incremental, inspectable knowledge view.

Real model runs compile newly accumulated visible events using the current
model; this incurs additional model usage. Ordinary steps only queue the latest
snapshot for a coalescing background worker and never await memory generation.
Background compilation starts at 12,000 new source characters, 32 source chunks,
or 8 user turns during execution; after an answer, the thresholds are 4,000
characters, 16 chunks, or 4 user turns. A 500-ms debounce combines nearby updates,
and background attempts are spaced at least 30 seconds apart. Successful batches
continue catching up while the conversation is idle. A failed background batch
gets at most one automatic retry; later activity can enqueue another attempt.
Model failure, invalid JSON or a 20-second background request timeout leaves the
previous wiki intact. The offline demo does not call a model for memory.

Before actual **summary compaction**, the runtime checks whether the wiki's
source cursor covers the proposed history boundary. If needed, foreground
catch-up takes over the background worker and reads canonical events, including
content omitted from normal tool projections. All catch-up batches and boundary
expansions share an 8-second / 4-batch budget per preparation (transport retries
remain subject to that deadline). Newly committed
notes are used in that same model request and the context budget is rechecked.
If coverage remains incomplete, optional automatic compaction is deferred;
mandatory overflow recovery and explicit compaction still proceed. Canonical
events remain available for later catch-up and verification. The remaining
memory work is queued again. Tool-output
pruning alone does not trigger this check. Disabling the plugin bypasses it.

Large events are processed in consecutive Unicode chunks, up to 4,000 characters
each and 24,000 per batch. A durable event/character cursor advances atomically
with each successful wiki revision. Budget reductions defer the remainder rather
than discarding it; failed batches do not advance progress. Restarting resumes
the cursor, and legacy excerpt-based notes are backfilled from the beginning.
Each background pass makes at most one compilation request; foreground catch-up
uses the shared budget above. Cancellation, shutdown and model replacement stop
background work. Complete ingestion does not guarantee that the model retains
every fact in its generated notes.

Compact supplies a self-contained handoff, including essential requirements,
decisions, failed attempts and concrete results as well as current progress and
the next action. Its correctness must not depend on optional Wiki notes.
Wiki archives more detailed decision reasons and
alternatives, failure conditions and verified fixes, concrete results/artifacts,
and source evidence. Later fixes should preserve earlier failure conditions and
decisions with their corrections. The overview is a topic guide for inspection
and tools, not a second rolling progress summary.

Normal context receives at most 2,400 characters of guidance, complete active
constraints, and relevant historical records/passages. The full overview and
unrelated page bodies are not automatically injected. Matching complete paragraphs
can come from anywhere in a page; paragraphs too long to fit remain available
through a page locator. Short continuation requests reuse the recent explicit
topic. A small directory keeps nonmatching archives discoverable; all pages,
records and original citations remain accessible through tools. Requirements are
never truncated to fit: omitted active constraints produce a reminder to read
the tasks tool. Legacy overview-only snapshots retain a matching excerpt or an
index link. This reduces duplicate prompt content without deleting stored notes
or relying on an unvalidated compact snapshot for semantic deduplication.
Its cost is included in the existing input budget. Structured task records distinguish
planned, attempted, completed, failed, superseded and uncertain work. Each
record has exact source quotes; completion requires an eligible actual tool
outcome (commands require a zero exit code), not an assistant's claim or a tool
request. Newer cited failures cannot be overridden by old successful evidence.
These checks establish provenance/status eligibility, not semantic proof of
every generated statement. Pages and overview remain fallible derived notes.

The read-only `session_memory` tool provides index, keyword search, paginated page
and task reads, and `source` to inspect complete cited canonical events. Source
reads are limited to the current session's cited visible evidence, paginated and
redacted. The inspector shows task states, original quotes and pending ingestion.
No embedding database,
tokenizer or provider token-count request is added. Small model windows and
prefix-bound provider state skip automatic memory injection to preserve their
budget/protocol constraints. Disabling the plugin removes the tool on the next
turn, immediately revokes tool reads, stops compilation and discards in-flight
updates; stored pages remain inspectable and can be reused when enabled again.

The Wiki switch does not disable Compact or original-history recovery. Real
conversations retain `search_run_history` and `read_run_history` independently of
the plugin; these tools are exposed after summary compaction or output clipping,
when details are absent from the active context. They read the canonical JSONL
for the current run within the runtime-selected boundary, with bounded outputs
and paginated reads, including after restart. Uncompacted conversations need no
extra history tool schemas. Queries run locally without an additional model
generation request; returned text still consumes context in the next model turn.
When evidence is missing, the agent is instructed to look it up or acknowledge
uncertainty instead of inferring that an omitted event never happened.
Original-history reads exclude prior Wiki lookup results, and Wiki compilation
excludes history lookup results. New Wiki citations must refer to original
observations rather than these retrieval copies. Existing archives remain
inspectable; neither filtering nor turning Wiki off deletes canonical events.

The authoritative wiki is stored under the application's `memories/` directory,
isolated by run ID, with regenerable `index.md`, `tasks.md`, `log.md`, and `pages/*.md` views.
Original event JSONL is never rewritten. Source fingerprints reject stale or
mismatched history. Notes are fallible historical context, not instructions or
proof that a planned action happened; current user corrections take precedence.
No automatic cross-session sharing is performed. Hidden reasoning and provider
state are excluded; recognizable API tokens are redacted on a best-effort basis.
This improves recall but does not guarantee lossless memory of every detail.

Settings includes a **plugins** section for installing a local directory or ZIP,
updating an installed package, enabling/disabling it, and uninstalling it. A plugin
bundles Skills and MCP configurations under one identity. Both portable root
`plugin.json` packages and the `.codex-plugin/plugin.json` compatibility layout
are supported for these components. Standalone personal/project skills and
manually configured MCP services continue to work.

Plugin files are copied into application-managed storage. Import records the
source, version and content hash; editing the source does not change the installed
copy. Updates must target the same plugin identity and publish through an atomic
registry change. Invalid packages leave the previous installation intact.
Skills use `plugin-id:skill-name` identifiers and MCP services receive stable
plugin-owned IDs, so components from separate packages do not collide.

Installation never starts commands, connects to MCP servers, or calls a model.
The plugin card lists bundled components, compatibility warnings and MCP
connection tests. Existing per-operation permissions still apply. Disable,
update or uninstall revokes that plugin's clients and pending approvals without
changing unrelated extensions. Installation and activation also check the combined
Skill capacity against personal skills and projects already loaded by the host;
later project changes report discovery diagnostics if they exceed the catalog limit.
Public marketplaces, automatic package-manager
installation, OAuth account flows, Codex account mappings and lifecycle hooks
are not included. See the [plugin format, examples and limits](crates/agent-plugins/README.md).

Settings includes **tool services** and **skills**. MCP services support local
stdio executables and Streamable HTTP, with connection tests and paginated
discovery of tools, resources and prompts. Conversations keep separate persistent
MCP sessions so multi-step tools retain state. External calls require explicit
approval in the conversation; MCP servers use their own permissions rather than
the project's built-in file sandbox. Disabling or changing a server revokes its
connections and pending approvals. Switching an unrelated skill preserves them.

Personal skills live in the application-managed `runtime/skills` folder, opened
from Settings. Project skills are discovered in `.agents/skills` and
`.openharness/skills`. A skill consists of `SKILL.md` with YAML `name` and
`description`, plus optional reference/script files. The model discovers metadata,
loads a selected workflow on demand, and reads supporting resources as needed.
Use `$skill-name` to select one explicitly. Skills do not bypass permissions or
execute scripts automatically. Refresh after installing or editing skills.

Configuration uses atomic persistence and revision checks. Neither saving MCP
configuration nor loading a skill sends a model request. MCP credentials are
referenced by environment-variable name; OAuth is not implemented. See
[MCP protocol support and limits](crates/agent-mcp/README.md),
[skill discovery and bounds](crates/agent-skills/README.md), and
[local document tools](crates/agent-local/README.md).

Skill tools appear only when at least one skill is enabled; MCP discovery and
dispatch tools appear only when a server is enabled. Individual MCP tool schemas
and full skill instructions are loaded on demand. A conversation refreshes its
tool definitions at the next user turn or resume, and holds them stable during
that turn. Disabling an extension revokes its execution permissions immediately.
History read/search tools remain deferred until context has omitted history.

## Development

Install Rust with rustup. `rust-toolchain.toml` selects stable Rust with rustfmt
and Clippy. Dependency versions are declared in the workspace manifest; each
crate enables the features it uses. Commit `Cargo.lock` with dependency changes.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

These checks also run in GitHub Actions. Add `--offline` to Cargo build and test
commands when the dependencies are already cached and network access is
unavailable.

To start the API on port 7788:

```sh
cargo run -p api --locked
```

## Module boundaries

- `agent-local`: permission-scoped file, document and approved command tools.
- `agent-mcp`: official MCP SDK transport, protocol negotiation and session lifecycle.
- `agent-skills`: bounded skill discovery, metadata and scoped resource reads.
- `agent-plugins`: local capability packages, validated imports and atomic installation registry.
- `api::extensions`: persisted configuration, per-conversation MCP clients,
  approval lifecycle and composition with the ordinary `ToolExecutor` interface.

- `agent-core`: agent configuration, ordered conversation types, opaque provider
  state, event types, and validated run transitions. It contains no vendor wire
  codecs, transport clients, tool execution, or storage implementation.
- `agent-adapters`: protocol-specific response codecs and replay validation for
  Chat Completions, Responses, Anthropic Messages, and Gemini generateContent.
  `WireProtocol::decode_usage` normalizes settled usage snapshots, including
  each protocol's cache and reasoning accounting.
  Transport adapters can reuse these helpers; a new protocol can implement the
  same model contract without changing the core.
- `agent-runtime/model`: the provider-neutral model interface; capabilities and
  stream settlement live in `model_capabilities` and `model_stream`. Production
  runtime code does not depend on `agent-adapters`.
- `agent-runtime/context`: context policy, checkpoints, compaction adapters,
  and event projection. A compactor owns its input-size estimate; a context
  manager receives borrowed history.
- `agent-runtime/history`: history access contracts, validated limits, the
  built-in JSON protocol, event-store lookup, and enable/disable behavior.
- `agent-runtime/storage`: the storage contract and separate in-memory and
  JSONL implementations. JSONL indexing stays with the JSONL implementation.
- `agent-runtime/runtime`: lifecycle and persistence coordination, the execution
  loop, and process-local cancellation controls.
- `agent-local`: execution-host file tools, permissions, command approvals and
  OS isolation, change journals, and conditional file undo. It does not depend
  on Electron or HTTP, and is composed into the API's model-neutral tool executor.
- `api`: local application service, run management, public UI projections,
  authenticated event subscriptions, HTTP routing, and process startup.
- `apps/desktop`: Electron lifecycle and a restricted preload bridge; React
  views consume typed run snapshots. TanStack Query owns server state, Zustand
  owns local UI state, and TanStack Router owns navigation.

Public runtime types are re-exported from the crate root. Split modules around
responsibilities and keep implementation helpers private to their module.
Keep protocol encoding and decoding together so cursors, schemas, and output
limits evolve together. Use Rust's standard naming and formatting conventions;
prefer names that describe behavior over generic `Fake` or `Recording` names.

Tests are organized by behavior. Shared support holds ordinary scripted models
and short-context fixtures. Tests for pending operations, cancellation, or
storage failures keep their special fixtures local so their behavior is visible.

## Extension contracts

Dynamic executors override `ToolExecutor::snapshot_definitions`. The runtime
captures one snapshot for each execution turn or manual compaction and uses it
for context preparation, accounting, and dispatch. It rejects calls to tools
absent from that turn's catalog. Executors must also check current permissions
when executing, so a captured catalog never overrides later revocation.
Provider prefix bindings remain enforced: changing a bound tool catalog cannot
silently discard or rewrite persisted reasoning state.

`ContextManager::prepare` accepts `ContextInputRef`; `prepare_owned` is a
convenience for callers that already own their input. It must not be used to
introduce an implicit full-history copy in the runtime. Context compactor input
estimation must describe the request the adapter will actually send.

Use the named `ContextPolicy::builder()` and `HistoryLimits::builder()` setters
to distinguish token counts, event counts, and Unicode character limits.
`build()` validates the combined configuration.

Custom history providers implement both budget-aware execution and
`ToolOutputProjector`. A projector identifies the tool names and output format
it owns, preserves valid structured output and pagination, or returns an error
when the budget is too small. The context layer does not infer a protocol from
built-in tool names. When preparing context directly, attach the projector with
`ContextInputRef::with_tool_output_projector`; the runtime supplies its history
provider automatically.

`AgentRun::is_running()` describes the durable lifecycle. A newly created or
restored run can be `Running` while no task is polling it.
`AgentRuntime::is_executing(run_id)` reports in-flight work owned by that runtime.
`max_steps: None` means no configured step limit; `Some(0)` prevents the first
model request.

The internal module split preserves crate-root imports. Custom integrations
using the previous interfaces need these changes:

| Previous interface | Current interface |
| --- | --- |
| `ContextManager::prepare(ContextInput)` / `prepare_borrowed` | Implement `prepare(ContextInputRef)`; owned callers can use `prepare_owned`. |
| `ContextCompactor::compact` alone | Also implement `estimate_input_tokens` using the adapter's actual input format. |
| `HistoryAccess::execute` alone | Implement `execute_bounded` and `ToolOutputProjector`; `execute` delegates with no caller-supplied limit. |
| Positional `ContextPolicy::new` / `HistoryLimits::new` | Use named builder setters and validate with `build()`. |
| Four numeric arguments to `EventStoreHistoryAccess::with_limits` | Pass a validated `HistoryLimits`. |
| `AgentRuntime::max_steps() -> u32` | Returns `Option<u32>`, preserving the configured limit. |
| `ModelRequest` struct literals | Set `max_output_tokens` and `provider_context` (`None` for unbounded legacy text requests). |
| Shared main/summary token counter | Configure `ModelContextCompactor::with_token_counter` and `with_budget` independently. |
| `ModelMessage` enum variants | Use `ModelMessage { role, content, provider_state, context }`; each `content` is an ordered `ContentBlock` list. |
| Separate assistant `ToolCall` messages | Keep calls in the same assistant content list; tool results remain subsequent tool-role messages. |
| `ModelResponse::{FinalAnswer, ToolCalls, InputRequired}` | Use ordered `content` plus `ModelFinishReason`; convenience constructors are `final_answer`, `tool_calls`, and `input_required`. |
| Core `ProviderProtocol` and `ProviderResponseState::validate_response` | Use adapter-owned format keys and `Model::validate_replay_state`; built-in codecs live in `agent-adapters::WireProtocol`. |
| Direct `ModelRequest::validate_provider_state` | This checks generic invariants only; use `Model::validate_request` or `validate_provider_state_with` to include adapter payload validation. |
| `RunEvent::ModelResponseReceived` literals | Supply `token_usage` (`None` for legacy/manual events). Existing serialized logs still load without it. |
| `RunStarted`, `InputReceived`, `RunResumed` literals | Supply `files: Vec::new()` for plain text. The optional persisted field holds explicit `TextFileReference` snapshots; older logs remain readable. Existing runtime string methods remain available; use their `_with_files` variants for references. |
| Raw `estimate_request_tokens` diagnostics | Use `measure_request(input, request)` for the same calibrated estimate and headroom used by the context policy. |
| Wrappers around a `ContextManager` | Forward `request_usage_baseline` to preserve its counter-bound durable estimate anchors; forward `compact_now` to expose manual maintenance. |
| Custom `TokenCounter` | Optionally implement a stable, versioned `estimator_identity` to retain calibration after history rewrites and restarts. Exact-prefix calibration still works without it. |

Cancellation drops locally owned model/tool futures and lets event persistence
finish. Adapters must clean up their resources on drop; already committed
external effects cannot be undone. Execution coordination is local to one
runtime instance. Concurrent JSONL writes require clones of the same store;
independent instances or processes must not write the same directory.

The run still retains its full durable event history in memory. Context
compaction limits model input, not the size of that in-memory event log.

## Internal model contract

The harness uses its own Rust conversation contract. It does not convert every
provider to one vendor's HTTP API. The execution loop calls `Model`, operates on
portable content, and leaves request encoding, response decoding, signatures,
wire field names, authentication, and provider compatibility rules to adapters.

A `ModelMessage` has a `ModelRole` and an ordered list of `ContentBlock` values:
`Text`, `Reasoning`, `ToolCall`, and `ToolResult`. An assistant can return reasoning,
text, and several calls in the same response, preserving their order. A
`ModelResponse` carries that list and a finish reason (`Stop`, `ToolCalls`,
`InputRequired`, or `Length`); usage and opaque continuation state belong to the
surrounding `ModelOutput`. The runtime validates the finished response before
persisting it or executing calls. Truncated output cannot authorize a tool.

Protected requirements and derived summaries use user-role messages annotated
with `MessageContext`; they must retain user authority when encoded. They are
historical context, not additional system instructions.

`Model::capabilities` declares execution-relevant behavior: adapter-defined
reasoning choices, whether native windows/compaction are supported, and whether
all assistant responses require replay state. Window sizes and output limits
remain in `context_metadata`. A reasoning option named `high` is not assumed to
have the same meaning across models. Field mappings and provider-specific
compatibility flags stay inside the configured adapter.

### Streaming and settlement

Adapters implement `Model::stream` with `BlockStart`, text/reasoning/tool deltas,
`BlockEnd`, cumulative `Usage`, and one final `Finish` event. Block indexes retain
output order even when deltas for open blocks interleave. Opaque replay state is
attached only to `Finish`, after all blocks are complete. The stream assembler
rejects mismatched deltas, unfinished blocks, and events after settlement.

Existing adapters can keep implementing `respond` / `respond_with_metadata`;
the default stream bridge emits the same canonical events for their completed
output. `run_until_blocked_with_stream` lets a caller observe presentation events.
Those events are tentative until the completed response is validated and durably
committed. A failed or cancelled stream does not persist partial reasoning as a
replayable response and cannot execute a partial tool call.

Adapters with a streaming-only transport can implement `StreamingModel` and wrap
it in `StreamingModelAdapter`. The wrapper forwards streaming, capabilities, and
replay validation, and assembles completed responses for callers of `respond`
or `respond_with_metadata`. It requires no placeholder non-streaming method.

## Context strategy

`CompactingContextManager` is the portable text-summary strategy.
`NativeContextManager` is an alternative for providers that expose opaque
compaction and continuation items. Choose one manager for a run; neither
manager deletes the durable events used by history tools.

### Budgets and protected requirements

Model adapters expose `Model::context_metadata()` with their provider/model
identity, default context window, optional maximum window, optional automatic
compaction threshold, effective-window percentage, and maximum output tokens.
The runtime passes these capabilities to both context strategies on each
request. Providers own catalog lookup and refresh; the harness does not embed
vendor model names or fetch a remote model catalog.

`ContextPolicy` preserves which values the caller explicitly configured. Each
preparation resolves the policy against the current model, so changing models
does not turn values derived for a previous model into permanent overrides.
A larger advertised maximum is used only when the caller explicitly sets a
larger window; oversized window overrides are capped by that maximum.

The resolved input limit is the smaller of:

- `window - output_token_reserve - safety_margin`;
- `window * effective_context_window_percent / 100` (normally 95%).

These are independent bounds, not two margins added together. Without an
explicit trigger, automatic compaction uses the smaller of the model's trigger
(if supplied), 90% of the selected window, and the resolved input limit.
Explicit triggers remain explicit and are rejected if they exceed the input
limit. Output reservations are capped by the model's maximum output tokens.
The default desired post-compaction size is 75% of the resolved trigger. The
preferred recent tail is 16% of the input space left after fixed instructions
and tool schemas, retaining complete steps. Configure another ratio with
`recent_tail_percent(1..=99)`; an explicit `recent_tail_tokens` takes precedence.

| Configuration | Input limit | Trigger | Desired size | Tail before fixed-cost adjustment |
| --- | ---: | ---: | ---: | ---: |
| No metadata: 128,000 fallback window | 117,760 | 115,200 | 86,400 | 18,841 |
| Model default window 1,000,000, effective 95% | 950,000 | 900,000 | 675,000 | 152,000 |

Both rows use the default 8,192 output reservation and 2,048 safety margin.
Actual tail budgets subtract the current instructions and schemas first.
Unknown adapters retain the fallback; adapters must advertise their real limits
to use a larger window automatically. `policy.resolve(model.context_metadata())`
exposes the effective configuration for inspection.

```rust
// Store this metadata in the model adapter and return a reference from
// Model::context_metadata(). Values below are an example provider contract.
let mut metadata = ModelContextMetadata::new("provider", "model-revision", 272_000)?;
metadata.max_context_window = Some(1_000_000);
metadata.max_output_tokens = Some(32_768);

// With no explicit override, the runtime uses the model's 272k default.
let automatic = ContextPolicy::default();
assert_eq!(automatic.resolve(Some(&metadata))?.model_budget().context_window(), 272_000);

// Opt in to the advertised larger window; trigger and target follow it.
let larger = ContextPolicy::builder()
    .model_context_window(1_000_000)
    .build_for_model(&metadata)?;
assert_eq!(larger.compaction_threshold(), 900_000);
```

The target is a desired size, not another hard limit. A reduced context that
fits the input budget remains usable when protected content or the newest
complete step prevents reaching the target. The soft trigger allows additional
growth above the protected baseline and the last text checkpoint size, capped
at the hard limit, to avoid repeatedly compressing an irreducible context.
The legacy `input_token_limit(n)` setter specifies an input-only budget and
clears output/margin reservations; known model limits still apply. Use
`model_budget` for a complete window with an explicit output reservation.

By default, the latest user input is protected verbatim. Older `RunStarted` and
`InputReceived` messages can enter a summary; their exact content remains in the
canonical journal and is available through history tools. Summaries preserve
requirements and corrections, and newer user input takes precedence. Set
`preserve_all_user_messages(true)` when every user input must remain verbatim.
Inputs retained in the active tail appear only once. Fixed instructions and tool
schemas are counted in every request. If the protected input alone exceeds the
hard budget, preparation returns an explicit error.

Under the soft trigger, the portable manager retains complete tool results.
When pressure reaches the trigger, it first prunes oversized tool results using
`max_tool_output_chars` and the tool's structured-output projector. It stores the
exact replacements in a derived checkpoint and measures the resulting request
again. A sufficient reduction avoids a summary call. Otherwise it summarizes
complete old steps and retains a recent tail. Persisted pruning survives restart,
cancellation, and summary failure; canonical events are never rewritten. A
failed optional summary can continue with a fitting pruned projection. Requests
with prefix-bound provider state require native compaction before any rewrite.

`ModelContextCompactor` resolves its own model metadata independently of the
main model. `with_budget` and `with_token_counter` override its configuration;
`resolved_budget()` exposes its actual limits. Its requests carry an output
cap, and both input and returned output are checked against its own budget.
Recursive batches use the compactor's input limit. Older summaries are derived
notes; newer evidence and explicit user corrections can revise them.

For a compatible same-model route, the manager offers the exact projected prefix
to `estimate_projected_input_tokens` and `compact_projected`. The model compactor
keeps instructions, tool schemas, and prior messages unchanged and appends a user
request for one `ContextSummary` JSON object. This preserves a cacheable prefix
where the provider supports caching; cache hits are not guaranteed. The request
must end at a complete tool-call/result boundary and pass the adapter's replay
validation. Its own budget includes the appended instruction and estimation
margin. A different route, unsafe replay, or oversized prefix uses sanitized
portable event batches instead. Custom compactors keep their batch behavior
through the new methods' defaults. Summary responses cannot execute tools.

The default `ApproximateTokenCounter` estimates one token per four UTF-8 bytes
and adds framing allowances. Counting is local: the runtime and provider examples
do not load model tokenizers or make separate token-count API requests. The
estimate covers instructions, tool schemas, messages, and retained provider
state, but cannot exactly reproduce every provider's encoding. Use
`ApproximateTokenCounter::conservative()` for the previous one-byte-per-token
fallback. Explicit `new(chars_per_token)` retains the calibrated character-ratio
mode. Native item costs always use their declared token count, never the length
of encrypted data.

Both context managers measure the initial projection and each candidate/fallback
used for budget decisions. `ModelContextCompactor` checks summary input against
its own budget with the same local estimation interface. Output reservations,
estimation margins, and bounded overflow recovery remain necessary because local
estimates can undercount unfamiliar content or protocol overhead.

The managers calibrate continuing text requests from the most recent successful
`ModelUsage`: reported input tokens for the exact previous request, plus the
configured counter's estimate of the replayed response and subsequent messages.
Generated output is not automatically future input: reasoning may be discarded
or ignored by a protocol. Retained opaque state supplies its own replay cost.
Cached input still occupies the window and is not subtracted or added twice.
Exact-prefix calibration requires the same run, model metadata, instructions,
tool schemas, and message prefix.

After compaction or pruning changes that prefix, a compatible durable raw
estimate can retain a conservative correction: previous reported input plus
the signed change in the local estimate, floored at the new raw estimate. This
requires the same model/protocol, fixed instructions, tool schemas, and versioned
counter configuration. The rewritten request is entirely estimated:
`measured_input_tokens` is `None`, and its whole prediction receives estimation
headroom. Changed opaque replay state, incompatible identities, and legacy
anchors without a raw estimate use the local fallback. A successful response
establishes a fresh exact-prefix baseline. Trying a candidate projection does
not destroy the original anchor. Provider-native windows keep their separately
reported item cost and are not calibrated a second time.
Adapters that omit model metadata still expose usage in tracing, but use the
configured counter without calibration because their model identity is unknown.

Successful response events atomically store usage, versioned request
fingerprints, and an optional raw estimate with its counter identity. The bounded
cache of 128 runs can be rebuilt from those events after restart or eviction;
it never duplicates prompt text. Legacy logs without usage fall back to local
estimates; older usage anchors still support exact-prefix calibration.
`ContextManager::request_usage_baseline` binds the durable anchor to the same
counter used by the manager. Wrappers should forward this hook. Custom counters
can opt into durable rewrite calibration with a stable `estimator_identity` and
can override `count_prefix_tokens` to avoid cloning long message prefixes.

`ContextManager::measure_request` returns a shared `ContextMeasurement`: raw
estimate, input prediction, measured prefix/native items,
unmeasured extension, estimation margin, decision count, and source. The default
margin is 15% of **only the unmeasured portion**. Configure it with
`ContextPolicy::builder().estimation_margin_percent(0..=100)`. The decision count
includes this margin, while the model budget
still reserves output capacity and its fixed safety margin separately. A
rewritten prediction can remain `provider_calibrated` while containing no
measured prefix; inspect `measured_input_tokens` rather than treating the source
label as proof of a complete measurement. Current sources are `provider_calibrated`,
`provider_native`, and `estimated`. Historical records with `provider_counted`
or `tokenizer_estimated` remain readable; these sources are no longer produced.
The desktop exposes these same values rather than recomputing a second count.
The offline DemoModel uses heuristic estimation. Configured provider models and
the opt-in OpenAI and DeepSeek evaluators compare local predictions with usage
returned by normal generation requests.

`ModelUsage.input_tokens` includes all cached input, and `output_tokens` includes
reasoning already present in the provider's output total. Neither cache nor
reasoning is added twice. Set `ModelContextMetadata::with_accounting_identity`
to a versioned protocol/replay-mode key when one model has several adapters or
thinking modes. The runtime derives a fallback from the provider context identity
when available. Disable input calibration with `with_input_usage_calibration(false)`
for usage that includes provider-internal prompts or multiple model calls;
these observations are retained as spending data without becoming input anchors.
The UI's observed totals cover successful responses with reported usage, not
unreported usage or the complete provider bill.

Context overflow recovery is separate from network retries. Adapters opt in
through `Model::is_context_overflow` (or `StreamingModel`); the core never
classifies errors by message text. Before any stream event has been emitted,
the runtime may ask `ContextManager::recover_overflow` for one durably smaller
request and retry within the same model step. Built-in managers bypass the soft
threshold for this recovery, retain user requirements and tool-call pairs, and
verify shrinkage with the same local counter. A retry cannot execute prior tools
again, and cancellation still drops model/compactor futures. Unsupported
recovery, unchanged context, partial streams, and a second rejection stop the run.

### Manual maintenance

`AgentRuntime::compact_now(&agent, &mut run)` performs context maintenance on a
settled run even below the automatic trigger. It returns whether a smaller
durable projection was committed and leaves the run's lifecycle status intact.
The summary adapter may make a model call, but no ordinary assistant turn or
tool execution is started. The operation shares the execution guard with model
work and input submission; it rejects active or durably `Running` runs.
`is_compacting(run_id)` exposes progress and `cancel_compaction(run_id)` cancels
maintenance without cancelling the conversation. Already saved pruning remains
valid. Repeating maintenance with no useful reduction returns `false`.

Applications that support ongoing conversations can enable
`with_continue_after_response(true)`. A normal model stop then waits for the next
user turn, and the step budget applies per user turn, including restored turns.
Manual compaction also works on completed runs without reopening them.

### Durable reasoning and response replay

Readable reasoning is a `ContentBlock::Reasoning`; it is separate from opaque
`ModelOutput::provider_state`. A provider may return replay state with no readable
reasoning. The runtime checks identity and prefix invariants, delegates raw
payload validation to `Model::validate_replay_state`, and commits the complete
canonical response and state atomically with `ModelResponseReceived` before any
tool effect. JSONL storage therefore restores both after restart without an
adapter memory cache.

The core envelope contains an open-ended format key, adapter version, identity,
replay scope, raw JSON payload, and extra replay token cost. The core neither
matches protocol names nor parses vendor fields. The separate `agent-adapters`
crate currently supplies these codecs:

| Codec | Persisted payload |
| --- | --- |
| Chat Completions | Full assistant message, including reasoning fields and ordered `tool_calls` |
| Responses | Entire ordered `output` array, including reasoning/encrypted items, message phases, and function calls |
| Anthropic Messages | Assistant `role` and complete ordered `content`, including thinking signatures and redacted blocks |
| Gemini generateContent | Complete model `Content`, keeping every part and `thoughtSignature` on its original part |

`WireProtocol::decode_content` produces normalized ordered blocks;
`capture_state` retains the original payload, and `validate_replay_state` binds
it to the normalized message. Unknown fields and opaque items remain unchanged.
Unsupported executable item types fail explicitly. These are codecs and replay
contracts, not four production HTTP clients. Other adapters may own other format
keys and validators without registering them in the core.

Adapters opt into an exact `provider_context_identity` and declare required
replay through their capabilities (the existing `requires_provider_state` hook
remains a compatibility default). Include protocol version, relevant feature
flags, and account/tenant compatibility scope in the identity's `format` or
`provider`; never include credentials. The default validation hook rejects
opaque state until the owning adapter implements it. Call `Model::validate_request`
before direct transport use. When encoding saved state, replace the whole
assistant message with its payload; calls are already inside that message and
must not be encoded a second time. Tool results follow the assistant message.

New envelopes use version 1 and validate the ordering of visible text and calls.
Historical envelopes with no version deserialize as version 0 and retain their
old flattened-text/ordered-call validation. Legacy event records still load;
old prefix bindings are checked against their original message serialization
only when the current prefix is losslessly representable in that schema. This
fallback never drops new reasoning blocks or version 1 state to make a hash fit.

`Response` replay scope allows whole old steps to be summarized while preserving
retained responses exactly. `Prefix` also binds the producing request's system
instructions, tools, and prior messages. The adapter selects the scope using its
provider's compatibility rules. Portable summarization skips a soft trigger and
fails at the hard limit for prefix-bound state; use native compaction. Changing
a bound prefix, switching to an incompatible model, or loading required history
without state fails before a model request. Native windows carry a fixed
instructions/tools binding so replacing messages with a window cannot bypass it.

Portable event-batch compaction and history read/search omit opaque payloads
and readable reasoning. The cache-friendly same-model summary route can retain
provider state only as part of the unchanged, adapter-validated original prefix.
Opaque payloads are redacted from `Debug` output.
Canonical event serialization intentionally retains both; protect event storage
and raw subscriptions as provider conversation data. `replay_tokens` includes
replayed reasoning and opaque input cost beyond visible text/tool calls. The
built-in counter skips display reasoning when state supplies this cost, avoiding
double counting; without state, it estimates reasoning as text. Ciphertext or
signature length does not reveal its token cost. Adapters declare an estimated
replay cost using available usage and the protocol's actual retention rules;
reasoning excluded from later input contributes no replay cost.

Protocol references: [OpenAI conversation state](https://developers.openai.com/api/docs/guides/conversation-state),
[Anthropic preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking),
[Gemini thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures),
and [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/).

### Native provider integration

The runtime supplies the strategy and contracts; provider transport remains in
adapters. A native integration implements `NativeContextCompactor` and a `Model`
adapter:

- `ProviderContextWindow` contains the **entire ordered window**, its input
  token cost, and provider/model/format identity. Preserve retained messages,
  opaque items, and unknown JSON fields.
- `Model::provider_context_identity` opts into that exact identity. The runtime
  refuses to send native state to a legacy or incompatible adapter.
- Override `respond_with_metadata` to return usage and, when available, a
  complete updated window containing the request and response. Returning only
  the response's new items violates this contract.
- Mark `ModelOutput::context_compacted` when the provider compacted during the
  response. Protected user input is reinjected after compaction as historical
  user-authority context, following pending tool results. Ordinary full-window
  snapshots already include that protection and do not accumulate another copy
  each turn. Never promote this material to system/developer authority.
- Honor `ModelRequest::max_output_tokens` and use the compactor's independent
  input limit and requested target. Preserve a provider's returned window as-is.

The runtime observes model output after response text and all tool declarations
are durably recorded, before tool execution. Thus a native checkpoint covers
those declarations, while later tool results are appended exactly once.
Observation is cancellable; authoritative event writes still finish before
cancellation completes. The context managers validate checkpoint identity and
event-prefix fingerprints before reuse.

Built-in text and native checkpoint stores are in memory. Model-returned native
continuations are also committed in the canonical response event, including the
compaction marker. The native manager can recover the latest such window after
restart, covering the original tool declarations exactly once. Standalone native
compaction checkpoints remain derived data: use a durable checkpoint store to
avoid repeating a compaction that has not yet produced a model response.
Provider/model/format compatibility and prefix bindings are checked before reuse;
missing or incompatible required state is an error, never a text-only replay.

Usage-capable model adapters return `ModelUsage`. Runtime tracing records
estimated input, actual input/output, and cached input tokens; compaction
tracing records before/after/target sizes. The raw estimate remains available
for comparison with the provider's actual usage. Keep stable instructions and tool definitions
where possible, and measure total cost and latency rather than compression
ratio or cache hit rate alone.

The context regression tests cover repeated and manual compaction, latest-input
and opt-in full-user retention, durable pruning, history recovery, model-dependent
budgets, calibration across history rewrites and restarts, cache-friendly summary
prefixes, independent summary budgets, native item preservation, checkpoint
compatibility, and cancellation before tool effects. They use deterministic
adapters to verify these contracts; real-model task completion and semantic
summary quality still require provider-backed evaluations.
