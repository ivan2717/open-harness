# Session wiki memory

This crate stores bounded, derived notes for **one run at a time**. It does not
call a model, start background jobs, load plugins, or change the canonical event
journal. The host owns compilation, enable/disable settings, and authorization.

`WikiStore::apply(run_id, events, through_sequence, update)` requires the complete
contiguous canonical journal prefix. The run ID is supplied by the trusted host:
event envelopes do not carry run IDs themselves. Each page cites visible source
event sequences; reasoning-only responses, model-request bookkeeping, future
events, and `session_memory` retrievals cannot be cited. A source citation records
provenance, not proof that the generated claim is correct. Tool requests document
intent; success/failure events document outcomes.

On every update the store checks the previously compiled journal fingerprint.
The host must also call `validate_source(run_id, events)` before injecting stored
memory into a recovered run. Render the returned immutable snapshot with
`WikiState::overview(max_chars)`, and use `query_validated(run_id, events, arguments)`
for tool reads; both avoid reloading a different snapshot after validation.
A changed or truncated
journal fails validation instead of reusing stale notes. All original event
content remains in the canonical store; SHA-256 fingerprints are streamed without
allocating another serialized copy of a large journal.

Updates replace the overview and upsert stable page IDs. Omitted pages are
preserved. The compiler must include the old page body when asking a model to
revise it. `apply_progress` commits the generated notes together with a
`source_cursor` identifying the next unprocessed event and Unicode character
offset. A long event may span multiple revisions without skipping its middle.
Replaying the same update at the same cursor is idempotent; a different update
at that cursor or a backwards cursor is rejected. The simpler `apply` method
marks its entire supplied event prefix as consumed.

`through_sequence` is the highest canonical prefix verified by the snapshot,
not a claim that every character has been ingested. It can stay unchanged
while a long event is consumed or legacy notes are backfilled. Older snapshots
without `source_cursor` remain readable; hosts must backfill from the beginning
because their former excerpt boundaries did not represent complete coverage.
The journal fingerprint still covers complete canonical events, including any
characters awaiting ingestion.

Structured task items preserve goals, constraints, decisions, work status and
questions with exact source quotes. They are upserted by stable ID together with
pages and ingestion progress. Their previous versions are retained in the same
bounded revision history, and source evidence is checked again before resumed
memory is exposed to a model.

Writes are serialized with a
process mutex and a per-run OS file lock, then committed by atomic replacement.

## Storage and bounds

Each run uses a hashed directory under the host's chosen local data directory:

```text
<hash>/
  wiki.json       authoritative snapshot
  index.md        derived overview and page links
  tasks.md        derived task states and exact evidence quotes
  log.md          derived recent revision index
  pages/<id>.md   derived page body and source event sequences
```

Run IDs never become paths. Page IDs are lowercase ASCII slugs. Store/run/page
directories and state/output files reject symlinks; Unix file opens use
`O_NOFOLLOW`. The application data directory is assumed to be controlled by the
host, not concurrently renamed by an adversarial local process.

The JSON file is committed first. Markdown is a regenerable view: an export
failure does not roll back a successful JSON revision. `repair_exports` retries
export and surfaces errors to diagnostics. A subsequent successful update also
regenerates exports. Invalid/corrupt JSON is reported, never silently replaced.

Limits: 4,000 Unicode characters in the overview; 8,000 per page; 64 pages total;
8 changed pages per update; 64 distinct source references per page; 64 recent
revisions. Each revision retains only the previous versions of changed pages,
without nested snapshots. The serialized authoritative file is capped at 32 MiB.
Older canonical evidence remains available in session history after the revision
window expires.

## Retrieval

`tool_definition()` exposes one read-only tool, `session_memory`, with actions:

- `index`: overview and paginated page metadata.
- `search`: up to eight space-separated keywords, OR matching with title boosts.
- `read`: a page body with character-based pagination and source event sequences.
- `tasks`: paginated goals, constraints, decisions, work states and questions,
  with status and exact source quotes.
- `source`: paginated canonical text of an event cited by current pages or tasks,
  available only through `query_validated`; hidden reasoning, provider state and
  memory tool echoes are excluded. Recognizable API tokens are redacted.

All results include bounded metadata and fit within 12,000 Unicode characters.
`next_offset` continues a result; passing the returned `revision` prevents mixing
pages from different snapshots. Index/search/tasks offsets count results, while
read/source offsets count Unicode scalar values. Retrieval has no arbitrary path, run ID,
network, embedding, tokenizer, or write capability. The host binds it to the
current validated session. `overview` is separately bounded for prompt injection;
the full wiki is not inserted into every model request.

Generated notes are historical evidence. Current user instructions and fresh
observations take precedence. Important claims should be checked against the
cited original events, and memory must never grant additional permissions.
