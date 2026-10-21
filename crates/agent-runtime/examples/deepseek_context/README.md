# DeepSeek live context evaluation

This opt-in example exercises the actual `AgentRuntime`,
`CompactingContextManager`, `ModelContextCompactor`, and `EventStoreHistoryAccess`
against DeepSeek. All supplied records are synthetic. It never sends source files
or workspace content.

Requirements: Rust workspace toolchain, Python 3 (standard library only), network
access, and `DEEPSEEK_API_KEY` in the environment. The key is inherited by the HTTP
transport; it is not passed in command-line arguments, saved, or logged.

```sh
cargo run -p agent-runtime --example deepseek_context --locked -- all
```

Other selectors: `probe`, `large`, `runtime`, `failure`, and `accounting`. Results are written to
`evals/results/deepseek-context-<selector>.json`. Running a selector again replaces
its report. An optional third argument adds a report label, for example
`runtime thinking durable-state`. Normal workspace tests never make network requests.

## Scenarios

- **Large input:** 22,000 synthetic records with exact markers near the beginning,
  middle, and end. The test requires the *provider-reported* input count to exceed
  128,000 tokens and verifies no unnecessary compaction occurred.
- **Near the full window:** 125,000 records using the same marker checks,
  requiring at least 800,000 provider-reported input tokens. Included by `all`
  and `large`; it keeps the model-advertised window and automatic threshold.
- **Repeated compaction and recovery:** four real model-driven tool rounds,
  rolling model-generated JSON summaries, exact user constraints, and recovery of
  a detail omitted from the middle of a tool result. Search must actually locate
  the original event; merely invoking a search tool does not count as success.
  The final answer is checked field by field. Canonical storage is also compared
  with the original full output independently of model success.
- **Summary failure:** a deliberately tiny 32-token summary output cap produces a
  real incomplete provider response. Below the hard input limit, preparation must
  retain the original context. Above the limit, it must reject preparation.
  Neither case may write a checkpoint or mutate canonical events.

The repeated-compaction scenario deliberately overrides the effective window to
32,768, trigger to 9,000, target to 7,500, recent tail to 6,500, and tool projection
to 25,000 characters. These are evaluation settings, not production defaults.
The summary model has a separate 65,536-token window and 2,048-token output cap.

## Adapter scope

The example discovers context capacity and maximum output from the real `/models`
response. It uses `deepseek-flash`, which the
[official model mapping](https://api-docs.deepseek.com/) currently maps to
DeepSeek-V4.1-Flash-1017. The returned catalog name is recorded without claiming
the server independently attests an immutable model revision.

The transport uses the official HTTPS Chat Completions endpoint, rejects
redirects, bounds each call at 180 seconds, and limits one process to 32 model
requests. It makes no automatic retries. `finish_reason=length` and other
incomplete responses are errors rather than successful partial answers. Usage
includes cached prompt tokens; the cached subset is recorded separately.

This is an evaluation adapter, not a production provider implementation. It uses
non-thinking mode by default; the optional thinking evaluation below adds a
durable per-response replay for DeepSeek's `reasoning_content`. Provider
native compaction, streaming, cancellation of the Python subprocess, distributed
execution, and transport retries are outside this evaluation.

The scenario unit tests use a clearly identified scripted model only to validate
fixtures and report checks. Their fabricated usage is never written as live
evaluation evidence.

## Thinking-mode evaluation

```sh
cargo run -p agent-runtime --example deepseek_context --locked -- all thinking
cargo run -p agent-runtime --example deepseek_context --locked -- runtime thinking-drop
cargo run -p agent-runtime --example deepseek_context --locked -- accounting thinking
```

`thinking` enables `thinking.type=enabled` and `reasoning_effort=high` on both
the main and summary requests. Thinking scenarios reserve 8,192 output tokens
for each model, including reasoning. The intentionally failing summary case
retains its 32-token cap. Reports have separate `thinking` / `thinking-drop`
filenames, leaving the previous non-thinking results untouched.

The adapter uses `agent-adapters::WireProtocol::ChatCompletions` to decode each
complete response into ordered `ContentBlock` values and capture the original
thinking assistant payload in `ModelOutput::provider_state`, including final
answers. Its `Model::validate_replay_state` hook owns wire validation; the runtime
never inspects Chat Completions fields. It declares its thinking choice and replay
requirements through `ModelCapabilities`.

The runtime persists canonical content and raw replay state together and projects
both back after restart. Encoding replaces the whole assistant message with its
original payload, preserving reasoning and preventing duplicate calls. Removed
whole steps stay in canonical history but leave the active context after text
compaction. This HTTP example returns settled responses through the default
stream bridge; it does not claim network SSE support.

The counter uses provider-reported reasoning tokens (or a conservative output
count fallback) as extra replay cost when tools are enabled. Readable reasoning
blocks are not counted again when that state cost is present. Without tools,
DeepSeek ignores historical reasoning, so the counter excludes that extra cost.
No ephemeral cache or fake empty reasoning field is used. Metrics contain only
counts and synthetic-marker presence, never the raw reasoning payload.

Offline tests separately cover JSONL restart, response-write failures before tool
effects, all four protocol codecs, summary/history redaction, ordered content,
stream settlement, custom adapter formats, and legacy/new prefix binding.
Only DeepSeek has a live HTTP adapter in this evaluation.

`thinking-drop` is a diagnostic control that deliberately omits replay. Although
the provider documentation describes an error for missing reasoning, actual
enforcement must be observed rather than assumed. A successful response alone
does not demonstrate that the omitted state was preserved.

`accounting` compares the corrected input-only baseline with the historical
`previous input + all previous output + estimated new message` formula against
the actual next no-tools request. Its `passed` field requires nonzero reasoning
usage and a smaller absolute error with the new formula. Approximate token
counts can still differ from provider counts.

Protocol references:
[model metadata](https://api-docs.deepseek.com/api/list-models/),
[Chat Completions](https://api-docs.deepseek.com/api/create-chat-completion/),
[thinking-mode requirements](https://api-docs.deepseek.com/guides/thinking_mode/).
