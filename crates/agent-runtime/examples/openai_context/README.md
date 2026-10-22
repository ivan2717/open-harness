# OpenAI context evaluation

Opt-in real API evaluation for the explicitly selected `gpt-6-luna` model. Uses
the same synthetic precision and runtime fixtures as the DeepSeek example;
it never uploads source files or workspace contents. It requires Python 3, the
Rust workspace toolchain, network access, and `OPENAI_API_KEY` in the environment.
The key is inherited by the HTTP subprocess, never written to argv or reports.

```sh
cargo run -p agent-runtime --example openai_context --locked -- probe none token-meter
cargo run -p agent-runtime --example openai_context --locked -- precision none token-meter
cargo run -p agent-runtime --example openai_context --locked -- precision high token-meter
cargo run -p agent-runtime --example openai_context --locked -- runtime high token-meter
```

Results go to `evals/results/openai-context-<effort>-<scenario>-<label>.json`.
Use a new label to preserve prior evidence. Each process allows at most 32
generation requests, with a 180-second timeout per request and no automatic
retries or redirects. Token estimates are computed locally; no extra counting
requests are made. Normal workspace tests make no model calls.

## Scope

- `probe`: one small response to verify access to the exact model.
- `precision`: six calls across English, Chinese, and code/JSON (30 KB per
  corpus). First responses and usage are saved to JSONL, then reopened with a
  fresh context manager before the follow-up. It reports raw, predicted,
  decision, and provider counts separately. `passed` verifies completed calls
  and durable calibration, not accuracy or sufficient safety margin. The first
  response's finish reason is mapped to `InputRequired` for this probe lifecycle;
  content, usage, and provider state are preserved.
- `runtime`: actual `AgentRuntime`, four model-driven business-tool rounds,
  repeated model summaries, and history search/read to recover a clipped fact.
  Every main request records its actual context measurement. Lower test-only
  thresholds force compaction; this does not test the physical 1M boundary.

`none` uses reasoning effort `none`; `high` uses `high` and checks for at least
one response with nonzero provider-reported reasoning usage. Output is capped at
256 tokens for plain precision and 8,192 for thinking. Runtime reserves 8,192
for both main and summary output in thinking mode, matching the DeepSeek run.

## Protocol and accounting

The adapter uses Responses with `store:false`, replaying complete ordered output
items through the runtime's durable `provider_state`. It does not use
`previous_response_id` or rely on server-side stored conversations. Encrypted
reasoning is opaque replay material; it is neither printed nor used as a text
token estimate. The adapter's declared extra replay cost uses reported reasoning
usage, with a conservative output-count fallback if that detail is absent.

Thinking requests explicitly use `reasoning.context=all_turns` so earlier
reasoning remains eligible for rendering. The effective reasoning settings are
recorded in sanitized observations. This is an evaluation choice: default
`current_turn` can exclude older reasoning while it remains in the wire payload,
so context accounting must not treat these modes as equivalent. Prefix estimates
still require empirical verification against provider usage.

Model access is checked through `/models`. If the catalog lacks window/output
capacity fields, the adapter uses the documented model limits and labels their
source rather than pretending the catalog supplied them. Reports retain the
actual response model string.

Usage comes from the Responses decoder: cached input remains part of input, and
reasoning remains part of output. Reports show count-only reasoning/replay
metadata. Context preparation uses the shared approximate counter and calibrates
unchanged request prefixes against usage from normal model responses. Estimated
new content retains safety headroom. There is no tokenizer dependency or separate
counting request. The precision fixture verifies that JSONL restoration recovers
the usage baseline and compares both raw and calibrated estimates with actual
input usage. A successful functional run alone does not prove exact counts,
overflow recovery, or general task quality.

Provider-native compaction, network streaming, real overflow recovery, retries,
and production-scale reliability are outside this bounded evaluation.

References:
[GPT-6 Luna](https://developers.openai.com/api/docs/models/gpt-6-luna),
[reasoning and stateless replay](https://developers.openai.com/api/docs/guides/reasoning).
