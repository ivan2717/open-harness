# Open Harness Desktop

A desktop workbench for the Rust harness, built with Electron, React, TypeScript,
Zustand, Tailwind CSS, TanStack Query, and TanStack Router.

## Navigation

The narrow left rail keeps **会话**, **插件**, **模型**, and **设置** available
throughout the app. Hovering or focusing an icon shows its label; the selected
section stays highlighted. **会话** returns to the last conversation or new-task
page, preserving each draft. Its history sidebar can be collapsed independently
and is hidden on management pages.

Inside the conversation sidebar, **会话** lists chats without a project and
**项目** groups project chats beneath their folder. Each chat appears once.
Folders expand in place; their new-chat control starts a task in that project.
**添加项目** uses the native directory picker. Empty selected projects remain in
the list, and identical folder names remain distinct by full path. Search matches
chat content or the project name/path and reveals matches inside collapsed groups.

Project and general task drafts are separate, including unsent file references.
The top **新建会话** action and Cmd/Ctrl+N start a general chat; each project's
new-chat control opens and focuses that project's draft with read-only permissions,
without opening a directory picker or creating a run before the first message is sent.
The main process remembers directories selected through the native picker, including
empty projects. They can be reopened directly after restart; existing projects from
older versions are recognized from the local runtime's conversation history.
Renderer sidebar metadata alone cannot grant directory access. Merely viewing
project history never selects a directory or promotes its execution permissions.
This follows the project/chat hierarchy in the
[Codex projects documentation](https://learn.chatgpt.com/docs/projects).
**插件** contains separate tabs for packages, standalone Skills, and MCP services.
**模型** owns model configuration; **设置** contains interface preferences, local
data information, and the optional developer demo.

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

**设置 → 开发调试 → 运行上下文演示** explicitly starts a deterministic,
offline demonstration. This section is collapsed by default. The demo uses the
real `AgentRuntime`, tool execution, event persistence, and context compaction;
its generated text and tool data are synthetic and token counts are estimates.
It does not make model API calls, access a selected project, or change the default
model, project selection or task draft. Its small context window makes compaction
observable without changing production defaults. Existing demo history remains
readable and can be continued as a demo.

In **模型 → 接入模型**, configure a model using Chat Completions, Responses,
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
project, permissions and draft survive that navigation. Use **新建会话** in the
conversation sidebar to start a task without a project, or the project row’s new-chat
control to start another task in that folder.
Project access defaults to **只读**. Select **允许修改** in the permissions menu before creating the session to
authorize file edits within that project. The agent can list, read and search
files, load project instructions, and make guarded edits. The renderer cannot
start work in an arbitrary path without native directory selection.
Existing conversations display their bound project, permission and model as
fixed values; starting a new task is required to change that execution context.

Use **＋ → 添加文件** (hover or focus for its “添加文件或使用技能” hint), drag local files into the composer, or copy files in the
file manager and paste with Cmd+V / Ctrl+V to add references. Ordinary text pastes
remain text. References appear as removable filename chips, survive settings
navigation, and remain in the draft if sending fails. Unsent file snapshots are
kept only for the current app session; sent snapshots are saved in the run log.
Text and code files must be valid UTF-8: up to 8 files, 256 KiB each and 512 KiB
combined per message. Directories, symlinks, images, PDF and other binary formats
are rejected explicitly. Files can be sent without a project or accompanying text.
Attaching a file does not grant access to its parent directory or permission to
modify it. The model reads the captured content, even if the original later changes.
Snapshots participate in token estimation, context compression and history search;
the chat displays filenames and sizes without expanding their full content. Open
a sent reference to preview the exact saved text, even if its original path no
longer exists.

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

## Rich answers and output files

Assistant answers render Markdown headings, lists, quotes, GFM tables, task lists,
links, and fenced code. Code blocks support copying and syntax highlighting.
User messages remain literal. Streaming answers and saved history use the same
renderer; the persisted conversation keeps the original Markdown source.

Local Markdown images and file links resolve relative to the conversation's
project (absolute paths inside that project also work). Images can be enlarged;
audio and video use native controls without autoplay. Text/code files have an
in-app preview; other files offer native open, reveal and save actions where
appropriate. Missing files and unsupported previews show a readable fallback.
Remote media is loaded only on request; ordinary web links open in the browser.
Local resources are limited to 100 MiB each, with text previews up to 256 KiB.
Audio/video playback depends on Electron's supported codecs. PDF and Office
documents currently open in the system application rather than an embedded viewer.

Resource access is separate from Markdown rendering. The main process checks the
run, message and project boundary, copies bounded local resources into its private
cache on first resolution, and serves them through opaque resource URLs. Cached
resources survive app restarts and later source edits/deletions. A file that has
never been resolved is still dependent on its original path. User-input previews
come from the authoritative event snapshot rather than rereading that path.
HTML and executable files are never executed by the message renderer. Input
attachments remain UTF-8 snapshots; output previews do not add model vision,
audio/video understanding, or native provider file-generation APIs.

The design follows [DeepSeek Harness's separation of assistant Markdown and
resource rendering](https://github.com/deepseek-ai/deepseek-harness/tree/master/packages/client/ui-primitives)
and [session-scoped file access](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/api/workspace-files/README.md).
Its [document previews](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/client/ui-sidebar-documentpreview/README.md)
use transient preview data; our persistent local snapshots are an additional
choice. [OpenAI's public file workflow](https://learn.chatgpt.com/docs/artifacts-viewer)
also separates conversation output from file preview/open actions. These
references inform the boundaries; we do not assume a particular private desktop
renderer implementation.

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
assets. Assistant Markdown renders through React with raw HTML disabled; user and
tool text remain literal. Native
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
npm run test:e2e:files
npm run test:e2e:rich
npm run test:e2e:plugins
npm run test:e2e:navigation
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

`test:e2e:navigation` checks the persistent rail, selected section, hover and
keyboard hints, extension tabs, separate drafts, return to the previous chat,
sidebar collapse, project/chat grouping, duplicate folder names, search, isolated
project drafts and file references, native selection, direct project reopening
after restart, legacy history migration, navigation during delayed project lookup,
rejection of forged sidebar metadata, and the 1000px minimum window.
It saves screenshots and uses isolated local data without provider API calls.

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

`test:e2e:rich` verifies streamed Markdown, code copying, image enlargement,
actual audio/video playback, ranged resource reads, opt-in remote images, native
file action routing, and restart recovery after source files are deleted. It uses
an isolated project and local model fixture, without provider API calls.

The 2026-10-21 dependency audit reported no production dependency advisories.
The packaging toolchain still reports eight moderate entries in the transitive
`sprintf-js` logging dependency chain; its latest published version has no fix.
The lockfile retains the current `electron-builder` release instead of the older
release suggested by automatic audit remediation.

For an isolated development or test data directory, set
`OPENHARNESS_DESKTOP_DATA_DIR` to an absolute path before launching Electron.
Never point two application instances at the same directory.

## Tools and skills

The default project tool surface is `list_files`, `read_file`, `search_files`,
`apply_patch` and `run_command`. Read-only projects expose only the first three;
patches require write permission. Commands are available only when the system
sandbox is supported and require approval. Creating, editing and deleting files
share one patch interface, with review and undo preserved.

Open **插件** in the left rail. Use **安装插件** to choose a plugin folder
or ZIP through the native picker. Each plugin shows its version, included Skills
and MCP services, source, compatibility warnings, and connection checks. It can
be enabled, disabled, updated from a new package, or uninstalled as a unit.
Updating must keep the same plugin identity and preserves its enabled state.
Installing a package only copies and validates it; it does not execute scripts,
connect accounts, or call a model. Plugin tools keep the existing MCP approval
flow. Required credentials remain environment-variable references; the plugin
card lists those names and connection errors. Public marketplace browsing and
OAuth connections are not included.

Plugin Skills use qualified IDs such as `document-review:review` so packages can
share a skill name without replacing personal/project workflows. Their controls
appear inside the owning plugin card; conversation Skills show their provenance.
Disabling or uninstalling a plugin removes its contributed capabilities while
retaining independently configured Skills and services.

### Session memory

**插件 → LLM Wiki** is a built-in plugin enabled by default. It incrementally
archives a conversation's decision reasons, failure lessons, results and evidence. Automatic organization
uses the conversation's configured model and can incur additional model usage.
The Compact handoff remains self-contained: essential requirements, decisions,
failed attempts and results stay alongside current progress and the next action.
Only a bounded set of active constraints and relevant historical passages is
supplied automatically; full pages and source evidence remain available through
memory tools. The overview is a topic guide rather than a second progress summary.
Memory belongs to that conversation.
Ordinary turns queue background organization without waiting for it. Before actual
summary compaction, memory catches up to the proposed history boundary within an
8-second / 4-batch budget. Incomplete coverage defers optional compaction;
hard-limit recovery and manual compaction still proceed, retaining original history.
New notes are immediately included in the prepared request when its budget allows.

Open **运行详情 → 记忆** to inspect the saved overview and topic pages. Source event
buttons open the corresponding original event in the timeline. This view is
read-only: refreshing or reopening it does not request model generation. Generated
Markdown never loads remote images or opens file links automatically.

Turning off the plugin stops new organization and model access to its notes.
Compact and original-history recovery continue independently. After compaction
or tool-output clipping, the model can use `search_run_history` and
`read_run_history` to retrieve missing details from the current conversation's
original journal, including after restart. These local queries need no Wiki
compilation; their returned text still uses model input tokens.
Existing pages remain available in the memory inspector after the switch is off
and after restarting the app. The built-in plugin has no import, update or uninstall
action. Memory complements context compaction and the original event history;
derived notes can be incomplete, so source records remain available for verification.

**MCP 服务** and **技能** tabs remain available for standalone setup. Add a local executable
with one argument per line, or a Streamable HTTP URL. Enable, edit, remove and test
connections from the same page. Connection tests discover capabilities without
calling tools. Credentials are environment-variable references; OAuth is not yet
supported. Saving or refreshing configuration does not call a model.

MCP calls show the service, operation and arguments in a conversation approval
card. MCP services have their own filesystem/network permissions, outside the
built-in project sandbox. Per-conversation connections retain state; changing or
disabling a server revokes its sessions. Cancelled or interrupted external actions
are never automatically replayed. Configuration revisions reject stale updates.

Use **打开文件夹** to install a personal skill folder containing `SKILL.md`, then
refresh. Project `.agents/skills` and `.openharness/skills` folders are discovered
automatically. Manage standalone personal/project skills in **插件 → 技能** using
the scope selector; plugin-owned skills remain in their plugin cards. Enable/disable
switches are global for the exact skill identifier, not local to a conversation.
There is no permanent skill-management panel above the composer.
Use **＋ → 使用技能** to search enabled skills for the current draft or conversation.
Choosing one inserts `$skill-id` at the text cursor, preserving plugin namespaces;
it does not enable a skill globally, load instructions, create a run, or call a model.
Actual `load_skill` calls appear in the conversation as skill-loading records, with
failed loads and partial reads distinguished from a complete load. The model lists
metadata and loads selected instructions/resources in bounded pages. Loading
scripts does not execute them or expand permissions.
Skill tools are sent to the model only when an enabled skill is available. MCP
discovery/call tools are sent only when a server is enabled; individual service
tool definitions are loaded on demand. Each user turn keeps one tool-definition
snapshot, and configuration changes take effect on the next turn of the same
conversation. Disabled services still revoke access immediately.

Selected projects can read/search PDF, DOCX, XLSX and PPTX directly in read-only
mode through `read_file` and `search_files`. These detect the file format and
return source locations and continuation cursors for documents. Directory search
scans text and reports skipped document files; search a document path to inspect
its extracted content. Plain text
reads support files up to 32 MiB; edit/review limits remain independent. OCR,
native vision/audio input and full document layout rendering are not included.
The attachment picker still imports UTF-8 snapshots; use the project tools for
these document formats.

`npm run test:e2e:extensions` exercises real Electron IPC, the Rust API, document
fixtures, local Skills and a stateful stdio MCP peer. It covers settings CRUD,
discovery, lazy loading, permissions, approvals, cancellation and restart. MCP
and Skill enable/disable checks also inspect the actual provider tool schema on
the next turn of an existing conversation. MCP crate tests additionally cover
Streamable HTTP JSON/SSE and failure cleanup.

`npm run test:e2e:plugins` verifies native directory/ZIP import, package enable and
disable, namespaced Skills, explicit MCP connection checks, update failure
preservation, restart, and uninstall without removing standalone extensions.
It uses temporary packages and a loopback MCP fixture; no external services or
model API calls are required.

`npm run test:e2e:memory` uses a loopback model fixture and an isolated desktop
data directory. It verifies automatic Wiki generation, event provenance, read-only
inspection, disable/re-enable behavior, and persistence through unrelated settings
saves and an application restart. No real provider API or user configuration is used.
