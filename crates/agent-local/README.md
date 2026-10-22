# Local execution host

`LocalTools` binds a session to a canonical project directory. Its file tools
list, search, read, create and edit bounded UTF-8 files, and read/search PDF,
DOCX, XLSX and PPTX documents without commands or a Python installation. `ReadOnly` is the default;
`WorkspaceWrite` permits file edits within the selected project. Read-only
sessions advertise only read tools. `run_command` is advertised only when writes
are enabled and the platform has the supported sandbox. Execution checks still
reject calls to unavailable tools. Root/home/system
directories, protected credential paths, symbolic links and hard-linked files
are rejected. The root's identity is checked again on every operation.

Every edit saves its original contents and intended result before replacement.
Undo restores the session's original contents only if the file still matches
the last agent edit. It never resets Git or discards an intervening user change.
The journal lives outside the workspace, is bound to that workspace, and is
written atomically with owner-only file permissions.

Commands require individual approval. On macOS they run under `sandbox-exec`
with project-only writes, protected-path restrictions, a cleared environment,
and no network access. Other platforms currently reject commands because no
equivalent execution sandbox is implemented. Toolchains may require additional
setup when their build process needs caches outside the selected project.
There is no fallback to an unrestricted shell.

Commands are foreground operations: default timeout 60 seconds, maximum 120.
Cancellation and timeout terminate the owned process group. A trusted wrapper
watches a pipe from the host, terminating that group if the host crashes.
Starting background daemons, detached processes or new sessions with `setsid`
is unsupported: such processes can leave the supervised process group, although
they retain inherited filesystem/network sandbox restrictions. This is not a
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

## Document tools

- `read_document {path, unit?, offset?}` returns at most 6000 characters across
  at most 32 source units. `next_cursor` contains the next 1-based unit and
  character offset. Locations are PDF pages, DOCX paragraphs, PPTX slides in
  presentation order, and named XLSX sheets with explicit cell addresses.
- `search_document {path, query, offset?}` performs literal, case-sensitive text
  search, returns up to 50 matching lines with source locations, and provides
  `next_offset` when more matches exist.
- Both tools work in read-only mode and enforce the same canonical workspace,
  protected-path, symlink and hard-link policies as other file tools.
- `documents::extract_document(name, bytes)` exposes the same parser for callers
  that have already admitted a user attachment. It grants no filesystem access.

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
