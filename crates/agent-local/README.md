# Local execution host

`LocalTools` binds a session to a canonical project directory. Its file tools
list, search, read, create and edit bounded UTF-8 files. `ReadOnly` is the default;
`WorkspaceWrite` permits file edits within the selected project. Root/home/system
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
marks truncation. UTF-8 file reads are paginated; individual files are limited
to 256 KiB. File scans exclude protected paths and dependency/generated folders
(`node_modules`, `target`, `dist`, `out`, `.next`, `.venv`, `venv`, `__pycache__`,
`.openharness-tmp`). Scans are bounded by 4096 text files, 16 MiB of text, 16384
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
