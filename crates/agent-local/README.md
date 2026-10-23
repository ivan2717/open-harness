# Local execution host

`LocalTools` binds a session to a canonical project directory. Its public tool
surface is `list_files`, `read_file`, `search_files`, `apply_patch`, and
`run_command`. Reading/searching automatically handles UTF-8 text and PDF,
DOCX, XLSX and PPTX documents without commands or a Python installation. `ReadOnly` is the default;
`WorkspaceWrite` permits file edits within the selected project. Read-only
sessions advertise only read tools. `run_command` is advertised only when writes
are enabled and the platform has the supported sandbox. Execution checks still
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
