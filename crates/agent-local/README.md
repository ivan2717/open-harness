# Local execution host

`LocalTools` binds a session to a canonical project directory. Its public tool
surface is `list_files`, `read_file`, `search_files`, `apply_patch`, and
`run_command`. Reading/searching automatically handles UTF-8 text and PDF,
DOCX, XLSX and PPTX documents without commands or a Python installation. `ReadOnly` is the default;
`WorkspaceWrite` permits file edits within the selected project. The following
workspace restrictions apply to Ask/AutoReview; see Full access below for the
explicit host-execution mode. Read-only
sessions advertise read tools and sandboxed read-only commands. `run_command`
is advertised whenever the platform has the supported sandbox. Execution checks still
reject calls to unavailable tools. Root/home/system
directories, protected credential paths, symbolic links and hard-linked files
are rejected. The root's identity is checked again on every operation.

Every patch validates all files and hunks before writing, then saves original
contents, recreation permissions and intended results to the journal. Each
replacement is atomic; ordinary mid-batch failures conditionally roll back
already-written files. An external edit is never overwritten during rollback.
This is not a filesystem-wide atomic transaction: a process crash can leave a
partially applied batch, whose recorded intentions remain available for review
and undo. Empty directories created by a failed addition can remain.

Undo restores the session's original contents only if the file still matches
the last agent edit. Deleted files retain their original permissions when
recreated, including across application restarts. It never resets Git or discards an intervening user change.
The journal lives outside the workspace, is bound to that workspace, and is
written atomically with owner-only file permissions.

Commands within the configured project/delegated filesystem boundary run directly
when network access is denied. A command that requests network access requires a
host decision unless a matching grant already covers it. `ApprovalMode::Ask`
and `ApprovalMode::AutoReview` select who reviews a boundary request; neither safe
mode grants network access or removes sandbox restrictions. The host implements
the automatic reviewer and may apply a result only with the pending epoch.

On macOS commands use `sandbox-exec`; Windows uses AppContainer and ACLs. Both
limit writes to the approved project or command scratch directory, protect
credential/state paths and clear inherited secrets from the environment. Linux
command execution is unavailable until a supported OS sandbox is implemented.
There is no fallback to an unrestricted shell.

Command grants and pending requests share one root-conversation state lock.
Session approval publishes the grant before releasing all covered parent/child
requests; unrelated roots and broader requests retain their own approval.
Revocation cancels pending requests and invalidates approvals in transit, but
already-admitted commands must be stopped separately. Host-persisted grants can
be restored only through the host API, never through a model tool. Persistence
callbacks run under the same revision check and must not re-enter permissions.

Commands are foreground operations with an optional total deadline up to 86400
seconds. The initial output wait defaults to 10000 milliseconds (maximum 30000);
a yielded command is managed with `manage_command`.
Cancellation and timeout terminate the owned process group. A trusted wrapper
watches a pipe from the host, terminating that group if the host crashes.
Starting background daemons, detached processes or new sessions with `setsid`
is unsupported: such processes can leave the supervised process group, although
in sandboxed modes they retain inherited filesystem/network restrictions.
Full-access commands have no such inherited sandbox after detaching. This is not a
VM/container lifecycle boundary for arbitrary daemon workloads.

Command output retains its beginning and ending within 64 KiB and explicitly
marks truncation. UTF-8 file reads are paginated, with a 32 MiB input bound and
8192 characters per response. Edits and command-review snapshots retain their
independent 256 KiB file bound. Text search scans at most 64 MiB, reports skipped
binary/document/oversized/inaccessible paths and whether coverage is complete;
an empty match list never implies that skipped documents were searched.
Directory listings include file sizes and the appropriate reader.
File scans exclude protected paths and dependency/generated folders
(`node_modules`, `target`, `dist`, `out`, `.next`, `.venv`, `venv`, `__pycache__`,
`.openharness-tmp`). Command-review scans are bounded by 4096 text files, 16 MiB of text, 16384
entries and depth 64. Binary/oversized files and excluded folders are outside
the text change review.

Command snapshots expose changes for review but do not claim exclusive ownership
of those changes: automatic undo is disabled for command changes. An incomplete
post-command scan preserves its original baseline, reports `review_incomplete`,
and must reconcile before a new command can replace it. Restarted commands are
marked interrupted and never replayed automatically.

Run `cargo test -p agent-local` and
`cargo clippy -p agent-local --all-targets -- -D warnings`. The macOS command
integration tests require permission to create a Seatbelt sandbox; running
inside another restrictive sandbox can return `sandbox_apply: Operation not
permitted`. These tests use temporary directories and local loopback only;
no model API or external network access is required.

## Full access

The host may explicitly select `approval_mode: full_access`. Only a writable
root or an inheriting child without `write_paths` receives host execution.
Commands then run without a filesystem/network sandbox or another approval and
network defaults to allowed. Unix uses the same supervised process group;
Windows launches PowerShell directly inside the same kill-on-close Job. This is
an explicit policy branch, never fallback after isolation failure. Explicit
`access: read_only` or `network_access: denied` still selects the normal OS
sandbox, and unavailable isolation fails closed. Task read-only mode and exact
file leases remain authoritative even when the root uses full access.

Full-access file tools accept absolute and outside paths. Bounded reads and
atomic file replacement remain; protected filenames are accessible explicitly.
Only ordinary files in the project enter its review/undo journal: outside and
protected-file patches report `project_undo: false`. Arbitrary host-command
changes outside the project cannot be enumerated or undone by project review.
Switching back to Ask/AutoReview removes host-path and host-execution authority.

## Delegated runs

The host can bind every run in one project to a shared `WorkspaceAccess` using
`LocalTools::with_collaboration`. Pass `None` for the parent, `Some(files)` for a
delegate that may edit those exact files, or `Some(vec![])` for a read-only
delegate. These restrictions never widen the original permission mode. The host
must acquire the delegate's file lease before execution and stop/await its
execution before releasing that lease. Released tool handles cannot keep writing.

Lease acquisition normalizes workspace-relative paths and rejects protected
paths, symlinks, directories and overlapping reservations, including case aliases.
Parent edits and undo cannot touch files leased to another run. All patch targets
are authorized before any edit is applied, including legacy write/edit entry
points. Unrestricted delegates inherit the parent's workspace permission;
file-scoped delegates can execute only read-only sandboxed commands. Workspace
write commands are refused while any write lease exists. Read-only commands can
run concurrently; shared operation locks serialize workspace writes, lease
acquisition and release. Cancelling a lease
request while it waits creates no reservation.
`try_acquire` performs the same validation without waiting, returning `WouldBlock`
when an operation is active; use it while holding a short service admission lock.
Leases are in-memory coordination; they must be reconstructed deliberately after recovery, not inferred from old
tool results. Independent `WorkspaceAccess` instances do not coordinate with one
another or with external processes.

## Reading and searching

- `read_file {path, cursor?}` reads text or supported document formats. Pass
  `next_cursor` unchanged to continue; its `unit` is 1-based and `offset` counts
  Unicode characters, not bytes. Text files use unit 1, return at most 8192
  characters in `content`, and include the starting `line`. Documents return
  at most 6000 characters across 32 source units in `chunks`, with PDF page,
  DOCX paragraph, PPTX slide or named XLSX sheet/cell locations.
- `search_files {path, query, offset?}` performs literal, case-sensitive search.
  An individual file is dispatched to its text/document reader. Directories
  perform bounded text-only scans and explicitly report skipped documents;
  search those documents individually when relevant. Text search returns at
  most 100 matching lines; documents return at most 50. Serialized byte budgets
  may produce smaller pages. `next_offset` always reflects the retained matches.
- These tools work in read-only mode and enforce the same canonical workspace,
  protected-path, symlink and hard-link policies as other file tools.
- Legacy `read_document`, `search_document`, `write_file` and `edit_file` entry
  points remain available to direct `LocalTools` callers. They are absent from
  the runtime catalog and cannot be newly invoked by the model. Persisted runs
  retain their old results without re-executing calls. Legacy top-level read
  `unit`/`offset` fields remain accepted; they cannot be combined with cursor.
- `documents::extract_document(name, bytes)` exposes the same parser for callers
  that have already admitted a user attachment. It grants no filesystem access.

## Applying patches

`apply_patch {patch}` accepts the following small, explicit grammar:

```text
*** Begin Patch
*** Add File: notes.txt
+New file contents
*** Update File: src/example.txt
@@
 unchanged context
-old line
+new line
*** Delete File: obsolete.txt
*** End Patch
```

Each update hunk begins with `@@`; lines have a space (context), `-` (remove),
or `+` (add) prefix. Matching uses exact complete lines, ignoring LF/CRLF in the
patch itself. A hunk must match exactly once in the original file. Multiple
hunks must be ordered and non-overlapping. Add context to disambiguate duplicate
text. Insertions require context except in an empty file. Context lines preserve
their existing endings, added lines use nearby endings, and updates preserve
whether the file had a final newline. Added files use LF and a final newline.
Empty Add File sections create empty files. Paths must be unique and cannot be
ancestors of another target. Add refuses existing paths; update/delete require
existing text files. Limits are 1 MiB serialized arguments, 64 files per patch,
256 hunks per file, and 256 KiB per original or resulting file. There is no fuzzy
matching, rename operation or implicit full-file overwrite.

Parsers: [pdf-extract](https://docs.rs/pdf-extract/0.12.1/pdf_extract/),
[calamine](https://docs.rs/calamine/0.37.0/calamine/),
[quick-xml](https://docs.rs/quick-xml/0.42.0/quick_xml/) and
[zip](https://docs.rs/zip/9.0.5/zip/). XLSX uses streaming cells, so a sparse cell
at row 1000000 does not allocate a million-row matrix. Macros, external links,
formulas and embedded scripts are never executed; spreadsheet formulas use
stored values and are not recalculated.

Limits: 32 MiB source, 8 MiB extracted text, 20000 source units, 2000 PDF pages,
256 sheets and 200000 spreadsheet cells. Office archives are checked against
4096 entries, 8 MiB per XML part and 64 MiB total actual decompression before
parsing, with duplicate/unsafe names and XML document types rejected. PDF
object streams are bounded before text extraction (100000 objects, 16 MiB per
decoded stream, 64 MiB total); recursive page/form graphs are checked. These are
in-process resource guards, not an OS memory isolation boundary for third-party
parser internals. Unsupported PDF text compression/prediction returns an error
requesting a re-export instead of unbounded fallback decoding.

Text extraction is not a visual document renderer. Images/diagrams, Office
headers/footers, presentation notes and layout are not included. Empty PDF pages
report that OCR may be needed. Password-protected, malformed or oversized files
return an actionable error; the tools do not claim to have read their contents.
Fixtures in `tests/fixtures` are small deterministic PDF/OOXML files, regenerated
with the included standard-library Python script (test development only).
