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

The first version provides an explicitly labelled offline demonstration model
that exercises the actual Rust execution and context-management code. It makes
no model API requests. See [desktop setup and architecture](apps/desktop/README.md)
for state ownership, persistence, checks, and packaging.

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

- `agent-core`: agent configuration, ordered conversation types, opaque provider
  state, event types, and validated run transitions. It contains no vendor wire
  codecs, transport clients, tool execution, or storage implementation.
- `agent-adapters`: protocol-specific response codecs and replay validation for
  Chat Completions, Responses, Anthropic Messages, and Gemini generateContent.
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
preferred recent tail remains capped at 24,000 tokens (and shrinks for small
windows); increasing the window does not inflate every retention budget.

| Configuration | Input limit | Trigger | Desired size | Preferred recent tail |
| --- | ---: | ---: | ---: | ---: |
| No metadata: 128,000 fallback window | 117,760 | 115,200 | 86,400 | 24,000 |
| Model default window 1,000,000, effective 95% | 950,000 | 900,000 | 675,000 | 24,000 |

Both rows use the default 8,192 output reservation and 2,048 safety margin.
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

The text strategy preserves every user-authored `RunStarted` and `InputReceived`
message verbatim and in order, even after its event is covered by a summary.
Inputs still in the active tail appear only once. This preserves corrections
and constraints without relying on another model to extract them correctly.
Fixed instructions and tools are also counted. If those protected inputs alone
exceed the hard budget, preparation returns an explicit error instead of
summarizing them away. Long conversations with extensive user-supplied data
therefore still have a finite protected-input capacity.

`ModelContextCompactor` resolves its own model metadata independently of the
main model. `with_budget` and `with_token_counter` override its configuration;
`resolved_budget()` exposes its actual limits. Its requests carry an output
cap, and both input and returned output are checked against its own budget.
Recursive batches use the compactor's input limit. Older summaries are derived
notes; newer evidence and explicit user corrections can revise them.

The default `ApproximateTokenCounter` estimates one token per four UTF-8 bytes
and adds framing allowances. This is a heuristic, not an exact tokenizer;
non-ASCII and provider-specific encodings may differ. Supply a provider
`TokenCounter` for precise accounting, or use `ApproximateTokenCounter::conservative()`
for the previous one-byte-per-token fallback. Explicit `new(chars_per_token)`
retains the calibrated character-ratio mode. Native item costs always use their
declared token count, never the length of encrypted data.

The managers calibrate continuing text requests from the most recent successful
`ModelUsage`: reported input tokens for the exact previous request, plus the
configured counter's estimate of the replayed response and subsequent messages.
Generated output is not automatically future input: reasoning may be discarded
or ignored by a protocol. Retained opaque state supplies its own replay cost. Cached input still occupies the window and is not
subtracted. A baseline requires the same run, model metadata, instructions,
tool schemas, and exact message prefix. Compaction, changed projections, or a
model switch cannot reuse a mismatched baseline. Trying a candidate projection
does not destroy the original baseline; only selecting a new request commits
the change. Provider-native windows keep their separately reported item cost
and are not calibrated a second time.
Adapters that omit model metadata still expose usage in tracing, but use the
configured counter without calibration because their model identity is unknown.

These disposable hints use a bounded in-memory cache of 128 runs and do not
store conversation text. After restart, eviction, or invalid usage, accounting
falls back to the configured counter. Prefix estimates for the built-in counter
avoid cloning the long conversation. Custom counters may override
`count_prefix_tokens` for the same optimization.

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

Opaque payloads and readable reasoning stay out of text compaction input and
history read/search results. Opaque payloads are redacted from `Debug` output.
Canonical event serialization intentionally retains both; protect event storage
and raw subscriptions as provider conversation data. `replay_tokens` includes
replayed reasoning and opaque input cost beyond visible text/tool calls. The
built-in counter skips display reasoning when state supplies this cost, avoiding
double counting; without state, it estimates reasoning as text. Ciphertext or
signature length is not a tokenizer. Adapters can account for ignored reasoning
or use provider token-counting APIs.

Protocol references: [OpenAI conversation state](https://developers.openai.com/api/docs/guides/conversation-state),
[Anthropic preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking),
[Gemini thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures),
and [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/).

### Native provider integration

This repository supplies the strategy and contracts, not a vendor HTTP client.
A native integration implements `NativeContextCompactor` and a `Model` adapter:

- `ProviderContextWindow` contains the **entire ordered window**, its input
  token cost, and provider/model/format identity. Preserve retained messages,
  opaque items, and unknown JSON fields.
- `Model::provider_context_identity` opts into that exact identity. The runtime
  refuses to send native state to a legacy or incompatible adapter.
- Override `respond_with_metadata` to return usage and, when available, a
  complete updated window containing the request and response. Returning only
  the response's new items violates this contract.
- Mark `ModelOutput::context_compacted` when the provider compacted during the
  response. Protected user history is reinjected after compaction as historical
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

The context regression tests cover repeated compaction, verbatim constraints,
history recovery, model-dependent budgets, actual-usage calibration, independent
summary budgets, native item preservation, checkpoint compatibility, and
cancellation before tool effects. They use deterministic
adapters to verify these contracts; real-model task completion and semantic
summary quality still require provider-backed evaluations.
