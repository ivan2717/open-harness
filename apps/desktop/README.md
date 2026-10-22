# Open Harness Desktop

A desktop workbench for the Rust harness, built with Electron, React, TypeScript,
Zustand, Tailwind CSS, TanStack Query, and TanStack Router.

## Run locally

Install Node.js 24 LTS (minimum 22.12), npm, and the workspace Rust toolchain.
From this directory:

```sh
npm ci
npm run dev
```

The development command builds the Rust API, starts Electron and the Vite
renderer, and launches a private backend on a random loopback port. Closing the
application stops its backend. A normal desktop launch requires no separately
started server or API key.

Without an available real model, the new-task composer shows **添加模型**. Task
drafts remain editable and project selection remains available, but sending is
disabled until a model is configured. New tasks and the default-model settings
only list available real models; old preferences pointing to the demo are ignored.

**Settings → 开发调试 → 运行上下文演示** explicitly starts a deterministic,
offline demonstration. This section is collapsed by default. The demo uses the
real `AgentRuntime`, tool execution, event persistence, and context compaction;
its generated text and tool data are synthetic and token counts are estimates.
It does not make model API calls, access a selected project, or change the default
model, project selection or task draft. Its small context window makes compaction
observable without changing production defaults. Existing demo history remains
readable and can be continued as a demo.

In **Settings → 接入模型**, configure a model using Chat Completions, Responses,
or Anthropic Messages. Supply the provider's base URL, model ID and API key;
select the matching reasoning option explicitly. Context capacity and the
per-request output budget are automatic. **高级设置** offers optional overrides and
**恢复自动设置**; changing the model, endpoint or protocol clears those overrides.
Saving may read the provider's model metadata, but makes no generation or token
counting request. It selects the configured model for new conversations.
Real-model conversations support follow-up
messages and the harness's history-read tools; synthetic diagnostic tools are
only used by the offline demo.

The composer groups task setup in one place: its top strip shows the project,
its permissions when selected, and **此计算机**. **选择项目** opens the native directory
picker; the selected project's menu offers **更换项目** and **移除项目**.
The lower-right contains the model and send/stop controls. The text area grows with the draft,
supports Shift+Enter for a newline, and does not send during IME composition.
Menus support arrow keys, Escape with focus return, and outside-click dismissal.
**添加模型**, or **添加／管理模型** in the model menu, opens model settings; the selected
project, permissions and draft survive that navigation. Global workspace settings
remain in the sidebar. Use **新建会话** in the sidebar to start a separate project task.
Project access defaults to **只读**. Select **允许修改** in the permissions menu before creating the session to
authorize file edits within that project. The agent can list, read and search
files, load project instructions, and make guarded edits. The renderer cannot
start work in an arbitrary path without native directory selection.
Existing conversations display their bound project, permission and model as
fixed values; starting a new task is required to change that execution context.

Automatic limits are resolved in the desktop main process before they reach the
Rust runtime. Provider metadata takes priority over a small directory of verified
model IDs on official endpoints. Unknown models start with an 8,192-token window
and a 2,048-token output budget. Known models normally use an 8,192-token output
budget, bounded by their output capability and one quarter of the context window.
The output budget includes reasoning where the protocol counts it as output.
Metadata lookup is bounded and falls back when unavailable; manual values take
priority. The encrypted model configuration stores the final numbers so reopening
the app preserves the model identity and conversation budget without another lookup.

Commands require a separate one-time approval showing the exact command and
working directory. Supported macOS hosts run commands in an OS sandbox with
network access disabled and writes scoped to the project and temporary storage.
When the command sandbox is unavailable, commands fail closed; file tools remain
available. A read-only session cannot execute commands. Stopping a run cancels
pending approval and owned command processes. Quitting the application normally
first disconnects event subscriptions, then gives the runtime up to ten seconds
to stop active work and save cancellation records before a forced backend stop.

**改动与命令** opens file changes and command results on demand. File edits are
already saved in the project; the review shows their original and current
contents for supported, size-bounded UTF-8 text files. Binary files, oversized
files and excluded dependency/build directories are outside the review coverage;
an empty list does not prove that no other files changed. Undo restores a file
only when it still matches this session's last
owned edit, preserving changes that existed before the task. Command changes and
externally modified files are displayed but cannot be automatically undone when
ownership is uncertain. Review does not use `git reset` or discard a repository's
uncommitted work.

Commands are intended for foreground tasks such as builds and tests. Background
daemons and commands that detach into a new session are unsupported; lifecycle
cleanup does not guarantee termination of deliberately detached processes.

Electron keeps keys in its main process and encrypts saved configurations using
the OS credential facility (`safeStorage`). Keys are never returned by the bridge,
included in run snapshots, or saved in renderer preferences. Without secure OS
encryption, configuration works for the current app session without being saved.
Encrypted model configuration is restored into the backend's memory at startup.
The model identity excludes the key, so rotating a key preserves compatible runs;
changing a model, protocol, endpoint, or reasoning configuration creates a distinct
model configuration.

Create a run from the composer, inspect its streaming output and tool steps,
and use the context panel to examine budgets and actual compaction checkpoints.
Run history is stored in the application's user-data directory and survives
restarts. An interrupted run is shown as interrupted instead of silently
re-executing tools on startup. **继续任务**, or a new message in the same session,
explicitly resumes it while retaining its history and project. Already committed
tool effects are not automatically repeated during recovery. Completed, failed,
and cancelled sessions can also receive a follow-up in place.

An idle conversation offers **运行详情 → 上下文 → 压缩上下文**. The conversation header
contains the run details and project results controls. Compaction preserves original events, reuses the
normal context policy, and can be cancelled without ending the conversation. A
real model may make one or more summary generation requests. No separate token
counting requests are made. Input submission and model execution cannot race with
manual compaction; failed maintenance leaves the conversation available.

## Boundaries

- `src/main`: window lifecycle, backend child process, authenticated HTTP and
  event-stream connections, and a fixed IPC allowlist.
- `src/preload`: the typed, limited `window.harness` bridge. The renderer gets no
  Node.js APIs, arbitrary IPC access, backend token, or filesystem access.
- `src/shared/contracts.ts`: the JSON view models shared by the main process,
  preload, and renderer. The Rust API owns their server-side implementation.
- `src/renderer`: React views and TanStack Router routes. TanStack Query owns
  server data; Zustand owns local drafts and display preferences. Full snapshots
  have monotonic revisions so older HTTP responses cannot overwrite live state.
- `../../api`: the Rust application service around the existing runtime. Model
  protocols, tool effects, and compaction decisions remain in Rust.

Desktop navigation uses a hash history so routes also work from packaged local
assets. React renders model and tool text as text, never executable HTML. Native
provider replay payloads are excluded from the public UI projection.

## Checks and packaging

```sh
npm run typecheck
npm run format:check
npm test
npm run build
npm run test:e2e
npm run test:e2e:models
npm run test:e2e:workspace
npm run package:dir
```

`build` compiles the desktop JavaScript and renderer. `package:dir` additionally
builds a release Rust binary, places it in the desktop resources, and creates an
unpacked application for the current OS and architecture. `npm run package`
creates the configured distributable. Public distribution still requires the
appropriate platform signing and notarization configuration.
An already packaged app does not load changes from `out/`: rebuild with
`package:dir`, quit the old app and reopen the new bundle to see source changes.

`test:e2e` launches a real Electron window with isolated temporary data. It checks
creating and stopping runs, actual context compaction, restart recovery, and
renderer isolation, then saves screenshots under `test-results/`. It requires a
desktop display (or a suitable virtual display in Linux CI).

`test:e2e:models` uses a local HTTP fixture server for all three protocols. It
checks the settings form, credential redaction, multi-turn conversations,
manual compaction and cancellation, and encrypted configuration/checkpoint/usage
restoration after an Electron restart. It requires no provider key or paid API.

`test:e2e:workspace` uses a temporary project and local model fixture. It checks
the native directory grant, read-only protection, file changes, command approval
and denial, review and undo, explicit recovery after a backend crash, and normal
application shutdown while a command has child processes. Command
execution coverage currently requires macOS with the system sandbox available.

For a packaged-app check, set `OPENHARNESS_E2E_EXECUTABLE` to the packaged
executable and run `node tests/e2e/desktop.mjs`.

The 2026-10-21 dependency audit reported no production dependency advisories.
The packaging toolchain still reports eight moderate entries in the transitive
`sprintf-js` logging dependency chain; its latest published version has no fix.
The lockfile retains the current `electron-builder` release instead of the older
release suggested by automatic audit remediation.

For an isolated development or test data directory, set
`OPENHARNESS_DESKTOP_DATA_DIR` to an absolute path before launching Electron.
Never point two application instances at the same directory.
