# agent-providers

`HttpModel` implements `agent_runtime::Model` with an owned, cancellable `reqwest`
transport. Configure an API root (including its version path), an in-memory API
key, a deployed model name, its explicit context window, and its output limit.
Supported protocols are Chat Completions, Responses, and Anthropic Messages.

Image input is an explicit `ProviderConfig.supports_images` capability, never
inferred from the protocol or model name. `ToolExecutor::execute_output` returns
`ToolOutput { text, images }`; text-only executors keep using `execute`. Responses
uses native `function_call_output` image blocks; Anthropic embeds images inside
`tool_result`; Chat Completions sends attributed user image blocks only after all
pending tool text results. A direct request with images to a model configured
without visual input is rejected. The runtime instead clearly reports the omitted
image and keeps structural observations usable, before planning its input budget.

Tool images are limited to four per result, 5 MiB and 4,194,304 pixels per image,
with an 8192-pixel maximum edge. Supported header formats are PNG, JPEG, WebP and
GIF. Capture a bounded viewport or resize in the driver; coordinate-based tools
must retain the screenshot-to-screen transform rather than silently resize here.
The active context retains only the latest image-bearing tool result. Older
observations remain in the event store; text history/summary projections omit
image bytes explicitly. Each retained image reserves 8192 estimated input tokens,
with normal provider-usage calibration; this allowance is not an exact token count.
`JsonlEventStore` saves checked SHA256 image sidecars atomically and stores only
references in the JSONL journal. After successful persistence, `AgentRun` releases
older image bytes into content IDs. `EventStore::load_for_runtime` hydrates only
the latest group; older references receive path/type/size checks without reading
all historical rasters on every round. Full `load`/`load_range` restores image
bytes and verifies checksums. Missing references fail restoration; corruption
fails the actual image read. Checkpoint and Wiki source fingerprints use image
content IDs so releasing pixels does not invalidate source history. Provider
validation rejects unresolved references; hashes are never sent as image data.
Live event notifications also carry persisted content IDs, so a slow subscriber
cannot retain a queue of full screenshots; load their pixels by event sequence.

All response content, replay validation, and usage normalization go through
`agent-adapters`. Provider response state preserves reasoning signatures,
encrypted reasoning items, and function calls; it is never converted into user
text. Context metadata includes protocol and reasoning settings in its accounting
identity. This transport does not advertise native context windows/compaction and
does not make token-count or tokenizer API requests.

`ReasoningMode::Off` means no override: the model/provider default applies.
`Effort`, `DeepSeek`, and `AnthropicBudget` set explicit wire-specific options;
unsupported combinations are rejected. Anthropic thinking budgets may be reduced
for a smaller summary request. When a provider returns reasoning but omits its
reasoning-token breakdown, replay accounting conservatively uses total output as
an upper bound; observed input/output usage remains the provider's exact snapshot.

Text and reasoning deltas stream as they arrive. Chat Completions and Responses
also stream function argument fragments; Anthropic tool input settles as complete
JSON to preserve its canonical representation. No partial stream can settle or
execute tool calls without a protocol completion event. Dropping the request
future drops the connection. Connection-establishment errors and explicit HTTP
429/5xx responses retry at most twice; interrupted response streams and ambiguous
request timeouts are never replayed automatically.

Structured errors inside successful HTTP responses (including SSE failures) retain
their error category as `ProviderError::Rejected`. They are not transport retries.
Confirmed context overflow can enter the runtime's bounded compaction recovery;
partial output prevents automatic recovery. DeepSeek's generic HTTP 400 capacity
error is recognized only when its complete template and token arithmetic agree.

HTTPS is required except for loopback HTTP endpoints. Redirects are disabled.
Credentials are sensitive HTTP header values, configuration is not serializable,
and errors contain only structural classifications. Response bodies, URLs and
keys are not included in diagnostics. Tests use ephemeral loopback mock servers;
no real provider credentials or API calls are required.

## Opt-in context boundary evaluation

The `context_boundary` example sends deterministic synthetic records through this
production transport. It calibrates row count from normal generation usage, then
probes 90%, 98%, and 102% of an independently verified model window. It deliberately
bypasses context management to distinguish provider capacity from runtime policy.
Targets are extrapolations; only successful response usage establishes actual size.

With `DEEPSEEK_API_KEY` or `OPENAI_API_KEY` already supplied securely in the process
environment, run, for example:

```sh
cargo run -p agent-providers --example context_boundary -- \
  deepseek 1048576 evals/results/deepseek-boundary.json all
cargo run -p agent-providers --example context_boundary_runtime -- \
  deepseek 128431 evals/results/deepseek-runtime-boundary.json all 1048576
```

Choose runtime rows from the direct probe's calibration/report, rather than treating
the example number as a universal token count. The runtime example uses default
policy and tests protected-input rejection, historical summary with restore, and
pressure-based tool pruning. Its `overflow` mode exercises two actual conversation
turns and requires a real provider overflow followed by successful compaction and
continuation. Do not include API keys in command arguments or reports.

Direct reports cap model invocations at five, including an optional one-shot
synthetic wire diagnostic; runtime reports cap them at ten. HTTP retries follow
the transport limits above. Reports are saved before and after each request and
exclude full prompts and reasoning payloads. Diagnostic stages preserve only
bounded, sanitized provider error details. These examples are manual live tests,
not CI tests. See [the measured boundary report](../../evals/context-boundary-report.md)
for results and limitations.
