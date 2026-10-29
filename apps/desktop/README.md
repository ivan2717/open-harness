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

Install Node.js 24 LTS (minimum 22.12), pnpm 10.13.1, and the workspace Rust toolchain.
From this directory:

```sh
pnpm install --frozen-lockfile
pnpm run dev
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
or Anthropic Messages. Supply the provider's base URL, model ID and API key.
Reasoning controls prefer the connection's model metadata, then an editable
provider or protocol preset. Provider recognition uses the configured official
HTTPS host, not a list of model IDs. An unknown relay receives a generic protocol
preset and never inherits an official provider's dialect merely by model name.
Presets are labeled **默认预设** because they are compatibility suggestions, not
verified support for every model. The initial selection remains the service
default and sends no reasoning override. **高级设置 → 自定义当前选项** copies the
current choices for editing their wire format, levels and budgets.
Context capacity and the
per-request output budget are automatic. **高级设置** offers optional overrides and
**恢复容量自动设置**; changing the model, endpoint or protocol clears those overrides.
Saving may read the provider's model metadata, but makes no generation or token
counting request. It selects the configured model for new conversations.
Saved model cards offer **编辑配置**. The form loads the existing connection,
image capability, reasoning choices and limits; **保存修改** updates that profile
and its idle conversations rather than creating another entry. API keys are never
returned to the form: leave the password field blank to retain the saved key.
Changing to another service origin requires an explicit key for that service.
Active runs, compaction and commands must finish before editing their model.
History remains available after an edit; incompatible provider replay state is
excluded, and unsupported conversation reasoning overrides use the new default.
**取消编辑** leaves the configuration unchanged. Test this workflow with
`pnpm run test:e2e:model-edit` (isolated local fixtures, no paid model requests).
Real-model conversations support follow-up
messages and the harness's history-read tools; synthetic diagnostic tools are
only used by the offline demo.

The composer groups task setup in one place: its top strip shows the project
and **此计算机**. **选择项目** opens the native directory
picker; the selected project's menu offers **更换项目** and **移除项目**.
The lower-right contains the model and send/stop controls. The text area grows with the draft,
supports Shift+Enter for a newline, and does not send during IME composition.
Menus support arrow keys, Escape with focus return, and outside-click dismissal.
**添加模型**, or **添加／管理模型** in the model menu, opens model settings; the selected
project, permissions and draft survive that navigation. Use **新建会话** in the
conversation sidebar to start a task without a project, or the project row’s new-chat
control to start another task in that folder.
New conversations allow edits within the selected working directory. The three
permission presets control approval and access beyond that directory. Existing
read-only conversations retain their restriction until the user selects a preset.
The agent can list, read and search
files, load project instructions, and make guarded edits. The renderer cannot
start work in an arbitrary path without native directory selection.
An idle conversation's lower-right model menu can switch to another configured
model or adjust **思考强度** for the next turn. **默认** inherits that model's saved
configuration; selecting a different model resets the conversation override.
These choices survive restart without changing other conversations or saved
model defaults. The menu is locked during generation, compaction and saving.
Reasoning choices are model-specific: a connection can expose effort values,
an on/off switch, or explicit thinking budgets that fit its output limit.
The adapter maps those choices to Chat Completions, Responses or Messages fields,
including DeepSeek thinking and Anthropic adaptive/budget settings. A service
default sends no override; it is distinct from explicitly disabling thinking.
Advanced compatibility templates can retain their parameter mapping while the
default remains unchanged, so a later conversation override still uses the right
format. Custom choices can map a display ID to a different provider effort value.
Saving uses at most one bounded model metadata GET; it never tries generation
requests to discover supported levels. Manual capacities do not skip reasoning
discovery. Resolved choices are stored per connection with the encrypted model
configuration and reused on restart. Changing session settings makes no generation request.
Existing saved declarations are retained on restart, including declarations from
older versions; an upgrade does not silently change a conversation's model ID or
reasoning request. The raw Rust API accepts the same explicit capability contract;
without one it supplies protocol presets. Provider discovery belongs to the desktop
configuration layer, so the runtime does not maintain a duplicate provider catalog.
Switching preserves the visible conversation, files, Wiki and original event log;
the runtime passes earlier assistant/tool work as labeled historical context and
does not replay another model's private reasoning or signed state. New replies
retain strict provider-state validation. Context budgets use the newly selected
model. Offline demo conversations remain separate from real-model conversations.

Reasoning discovery uses documented, machine-readable model APIs. It does not
scrape documentation pages at runtime or probe generation requests. The adapter
coverage below was checked against official documentation on 2026-10-26:

| Service                                                                                 | Discovery and fallback                                                                                                                                                                                     |
| --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [OpenAI](https://developers.openai.com/api/reference/resources/models/methods/retrieve) | The Models API exposes identity fields, not effort choices; use editable effort presets.                                                                                                                   |
| [DeepSeek](https://api-docs.deepseek.com/api/list-models/)                              | Read `effort.supported_levels` from `/models`; map Chat, Responses and Messages separately.                                                                                                                |
| [Anthropic](https://platform.claude.com/docs/en/api/http/models/retrieve)               | Read effort and thinking capabilities from `/v1/models/{id}`. Prefer adaptive thinking when declared; otherwise use effort-only or explicitly supported budget mode.                                       |
| [xAI / Grok](https://docs.x.ai/developers/rest-api-reference/inference/models)          | Read `capabilities.reasoning_effort` from `/v1/models`; use effort presets when absent.                                                                                                                    |
| [Moonshot / Kimi](https://platform.kimi.com/docs/api/list-models)                       | Read the published model metadata, including `supports_reasoning`; a boolean alone does not establish effort levels, so those remain presets.                                                              |
| [Qwen / DashScope](https://help.aliyun.com/zh/model-studio/list-models)                 | Read `/api/v1/models?model=...` on the same regional host, including for Messages connections. Published capacity/capability fields do not supply exact effort levels; use editable compatibility presets. |
| [Google Gemini](https://ai.google.dev/api/models)                                       | For OpenAI-compatible connections, read the native same-host model resource for limits and `thinking`; effort levels remain presets unless explicitly declared.                                            |
| [OpenRouter](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)         | Read the single-model resource and `reasoning.supported_efforts`; honor `mandatory` by omitting disable choices.                                                                                           |

Failure, timeout or missing metadata falls back without blocking setup. Metadata
requests never send the configured key to another host or follow redirects.
Manual settings take priority over discovered data. A service's reported default
effort does not become a forced request parameter: **使用服务默认设置** leaves that
decision to the service.
Project and permission remain fixed; start a new task to change them.

Use **＋ → 添加文件** (hover or focus for its “添加文件或使用技能” hint), drag local files into the composer, or copy files in the
file manager and paste with Cmd+V / Ctrl+V to add references. Clipboard screenshots
can be pasted directly without saving a file first. Ordinary text pastes remain
text; mixed image/text clipboard content keeps both. References appear as removable
filename chips or image thumbnails, survive settings
navigation, and remain in the draft if sending fails. Unsent draft references are
kept only for the current app session; sent attachments are saved with the conversation.
Ordinary file attachments retain their original bytes in a local content-addressed store.
Identical originals share one copy. At startup, abandoned originals older than seven
days are collected only after a complete, validated history scan proves they are
unreferenced. Originals referenced by conversation history are retained; uncertain
or damaged history skips cleanup.
Their names, sizes and immutable references enter the prompt; their full contents do not.
The agent uses `read_file` / `search_files` on `attachment:<id>` to read or search the
needed sections. UTF-8 text and PDF, DOCX, XLSX and PPTX have built-in readers; the
current document parser accepts sources up to 32 MiB, independently of attachment
storage. Other formats remain available as original files and report their reading
requirements instead of being treated as text. No automatic OCR, macros, formulas
or external references run on import. This local flow does not use provider-native
`input_file` uploads. Older conversations containing extracted text snapshots remain
readable and retain their original snapshot/export behavior.
Up to 64 attachments may be added per message. Ordinary original files are limited
to 100 MiB each; the former 256 KiB per-file / 512 KiB total text limits apply only
to legacy inline snapshots, not new imports. Files above the storage limit are
rejected explicitly, never silently truncated.
PNG, JPEG, WebP and GIF images are decoded locally into PNG/JPEG snapshots
(animated images use the first frame). Each message supports up to 4 images
within the 64-attachment limit. Sources are limited to 32 MiB and 32 megapixels;
snapshots are resized to at most 4 megapixels and 8,192 pixels per edge, with a
5 MiB encoded limit. Images have an enlargeable preview and require a model with
**此模型支持图片输入** enabled. Their pixels are sent through the selected provider's
image-input protocol, never as text or a local path. Sent pixels live in
content-addressed sidecars; list responses, events and text history carry metadata
instead of base64. Directories and symlinks are rejected explicitly.
Files can be sent without a project or accompanying text.
Attaching a file does not grant access to its parent directory or permission to
modify it. The agent reads the saved original even if the source later changes or
is deleted. Attachment metadata and the portions actually read participate in
token estimation and context compression. The chat displays filenames and sizes
without expanding full contents. Sent references can preview, open or save the
captured original; legacy extracted-document snapshots export as text.

Automatic limits are resolved in the desktop main process before they reach the
Rust runtime. Provider metadata takes priority over a small directory of verified
model IDs on official endpoints. Unknown models default to a 1,000,000-token window
and an 8,192-token output budget; the UI labels this as an assumption, not a verified
provider limit. Known models normally use an 8,192-token output
budget, bounded by their output capability and one quarter of the context window.
The output budget includes reasoning where the protocol counts it as output.
Metadata lookup is bounded and falls back when unavailable; manual values take
priority. The encrypted model configuration stores the final numbers so reopening
the app preserves the model identity and conversation budget without another lookup.
Previously saved configurations keep their limits, including legacy 8K values:
older versions did not retain whether those numbers were automatic or manual.
Use **更新上下文容量** on an existing model to resolve its window again without
re-entering the API key. This keeps the output and reasoning settings, and migrates
idle conversations to the updated model configuration while preserving their history
and checkpoints. Active execution or compaction blocks the update. Both connections
are saved before migration; the old one is removed only after all bindings are updated.

Sandboxed commands with networking disabled run inside the selected project
permissions without another approval. Network requests show the exact command,
working directory, file access and network scope. **允许** reuses that scope in
this root conversation and its children; **始终允许** keeps it across restarts for
this same conversation and directory; **仅本次** is a single action. **取消** cancels
the pending request. The composer offers only **请求批准 / 帮我批准 / 完全访问权限**.
Saved grants, review decisions and token usage are secondary controls in settings. Changes to
policy require an idle task family. Revocation cancels pending approvals and
late reviews; **撤销并停止** also stops running work.

In the first two presets, macOS uses Seatbelt and Windows uses AppContainer with Job Objects. Networking
is disabled unless explicitly authorized. Read-only commands cannot modify the
project and receive their own writable temporary directory. Workspace-write
commands can modify the project and its temporary storage. Missing or failed
sandbox setup never falls back to unrestricted execution; unsupported platforms
expose file tools only. Read-only projects can run sandboxed read-only commands.
Stopping a run cancels
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

Real-model tasks have no default 12-step limit. They continue until an answer,
input request, error or cancellation; context compaction separately keeps the
model input within its window. API clients may set a positive
`execution_policy.max_steps` when creating a task. Reaching that limit displays
a neutral paused notice and **继续任务**, retaining the previous work. Resuming
starts a new step allowance. The desktop has no execution-policy settings form;
ordinary tasks use the unlimited default. The offline diagnostic demo remains
bounded.

### Collaboration within a task

For independent parts of a task, the agent can create delegated tasks using the
same model. A compact **协作任务** row appears below the parent conversation's
header; expand it to see task names and activity, open a task's transcript, or
stop an active child. While the parent page is open, the list refreshes once per
second only while the parent or a child is active; polling stops when all are idle.
A spinner means active work, and a green dot means idle. Opening a child
keeps the parent selected in the sidebar and offers **返回父任务**. Child detail
pages have no separate composer: send new directions in the parent conversation,
which coordinates messages and follow-up work.

Each child has a fresh context and independent persisted history. It receives
the task supplied by its parent, rather than a copy of the entire conversation
or the parent's private reasoning. Results return automatically, and the parent
waits for active children before normal turn completion. Stopping the parent also stops
its children. Stopping an individual child prevents the agent from automatically
restarting it. Restarting the app preserves relationships and pending results,
but never resumes interrupted work automatically.

Children use the same project checkout, read-only by default. A parent with
write permission may assign exact files through `write_paths`; active assignments
exclude conflicting edits by the parent or siblings and release when the child
finishes. Children can execute read-only commands for inspection and tests under
the OS sandbox; they cannot run workspace-write commands. Test output must use
the command's private temporary directory. Reads observe live files and can
overlap edits, so final verification must run after those edits settle. Root and
child tasks share session command grants, and the parent page shows pending
approvals and running child commands with their task names. Children cannot
execute MCP operations or computer/browser controls, and cannot delegate
further. The coordinator handles workspace-write commands after active file
reservations finish, and external actions through the normal approval flow. This provides local
coordination, not separate Git worktrees or a shared Teams task board.

Windows read-only commands grant access to existing project files without
inheriting read access to new files. Start another command after files are added
or atomically replaced; their paths are checked again before access is granted.

The concurrent-child limit defaults to 4. API clients may choose 1–8 through
`execution_policy.max_agents`; the parent does not count toward this limit.
Each run keeps its own context window. Recorded token usage is observation of
successful provider responses, not a cross-agent context size or a hard billing
budget. See the [execution-policy details](../../README.md#long-running-tasks-and-delegated-agents)
and [service integration tests](../../api/tests/collaboration.rs).

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
document attachments remain UTF-8 snapshots; image attachments use the model's
explicit vision capability. Output previews do not add model vision,
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

### Windows CI installer

The GitHub Actions **Checks** workflow runs only on pushes to `main`. Changes to
the protected `main` branch must go through a pull request, including changes by
administrators; a second reviewer's approval is not required. CI validates the
merged commit. Other branch pushes, pull request updates, tags, and manual
dispatch do not trigger the workflow.

Rust, desktop, and plugin checks run on Linux for every update to `main`.
**Windows checks** also runs on every update, independently of version changes.
It tests native command permissions, sandbox enforcement and process lifecycle,
API execution and shutdown, the Chrome host launcher and named-pipe transport,
and desktop runtime discovery and Windows paths. These baseline checks do not
download Chromium or package the app. The Linux browser plugin tests install
headless Chromium and use local fixtures; no provider API keys are required.
**Windows x64 installer** runs only when the Linux and Windows checks pass and
`apps/desktop/package.json` has a different version than before
the push. Ordinary code, documentation, and dependency updates keep running
checks without creating an installer. Different main commits do not cancel each
other's workflows, so a subsequent ordinary update cannot cancel a version build.

When a version is ready to publish, edit `version` in `apps/desktop/package.json`
and submit it through a PR to `main`. The pnpm lockfile tracks dependencies and
does not repeat the application's version; update it only when dependencies change.
The workflow compares the complete push's before/after versions, including
multi-commit merges. An unreadable baseline fails the desktop check, and
`pnpm install --frozen-lockfile` checks dependency/lockfile consistency.
After the installer passes its smoke test, a separate publish job creates
`v<version>` at the exact build commit and publishes a GitHub Release. Only this
job has `contents: write`; build and test jobs keep read-only repository access.
Versions with a prerelease suffix, such as `0.2.0-beta.1`, publish as prereleases.
Ordinary merges with no desktop version change skip both packaging and publishing.

The Windows job builds the Rust backend and Electron app on a Windows x64 runner.
The release backend statically links the Visual C++ runtime. The installed
`api.exe` and the Chrome host launcher are inspected with `dumpbin` to reject separate VC runtime DLL
dependencies that a Visual Studio-equipped runner could otherwise hide.

The Windows job creates an NSIS installer, installs it into a temporary directory
with spaces in its name, and launches the installed executable with a fresh data
profile. It checks the IPC/backend connection, an offline task with tool calls and
context compaction, renderer isolation, and persisted history after restarting.
It also prepares the real current-user Chrome registration, starts the packaged
native host and MCP launcher, and tests the extension in an isolated Chrome for
Testing profile. This covers authenticated named-pipe communication, background
tab groups, page input/clicks and screenshots without switching the user's tab.
The same installed-app run prepares and exercises the independent Chromium
browser, then drives a temporary WinForms app through the packaged Windows
Computer Use helper. It checks Unicode input, ref clicks, navigation, scrolling,
window-only PNG capture, stale snapshot/password refusal, and owned process
cleanup. It then uninstalls the app before uploading the installer. This is a smoke test,
not full coverage of every Windows feature or an upgrade test.

Download `Open-Harness-<version>-windows-x64-setup.exe` directly from the
[Release page](https://github.com/ivan2717/open-harness/releases). Each release
also contains `SHA256SUMS.txt` and `build-info.json` with the source commit and
workflow URL. Release assets do not use Actions artifact retention.

Publishing verifies the commit, version, and installer checksum, uploads all three
files to a draft, and makes it public only after verifying their remote hashes and
sizes. Retrying the publish job resumes missing draft assets or accepts an already
published identical release; it never moves an existing tag or overwrites conflicting
assets. Reuse of a version for different content fails explicitly. Rerun the failed
publish job to retry the same build, rather than rebuilding an existing version.

The run's **Artifacts** section also provides
**Open-Harness-windows-x64-<run number>** for 14 days; smoke screenshots and logs
are retained for 7 days. Only a fully successful build and installed-app smoke test
uploads an installer.

These are **unsigned test builds**. No Apple account, Windows certificate, model
API key, or publishing secret is needed. Windows can show an unknown-publisher or
SmartScreen prompt. The installer is per-user and does not request administrator
elevation. Publishing to GitHub Releases does not add an application auto-updater.

To build the same installer locally on **Windows x64**, install Node.js 24, Rust
stable, and Visual Studio C++ Build Tools, then run:

```powershell
cd apps/desktop
pnpm install --frozen-lockfile
pnpm run package:win
# Optional (PowerShell 7): install, smoke-test, and uninstall with a temporary profile.
pwsh -File scripts/test-windows-installer.ps1
```

`package:win` refuses other host platforms/architectures so a macOS/Linux Rust
binary cannot accidentally be bundled with Windows Electron. Local packaging does
not publish a GitHub Release. The existing macOS local packaging command
remains available without an Apple developer account.

Windows currently supports the desktop conversation interface, model connections,
context management, history, local file tools, Chrome connection, the independent
Chromium browser and native Computer Use. Native desktop input requires an
unlocked interactive Windows session, stays in the selected foreground window,
and does not elevate privileges. Chrome tab operations can run in the background.
Sandboxed shell commands still require separate platform work; enabling these
plugins does not enable shell execution.

### Local checks and macOS packaging

```sh
pnpm run typecheck
pnpm run format:check
pnpm test
pnpm run test:packaging
pnpm run build
pnpm run test:e2e
pnpm run test:e2e:models
pnpm run test:e2e:workspace
pnpm run test:e2e:files
pnpm run test:e2e:rich
pnpm run test:e2e:plugins
pnpm run test:e2e:navigation
pnpm run package:dir
```

`build` compiles the desktop JavaScript and renderer. `package:dir` additionally
builds a release Rust binary, places it in the desktop resources, and creates an
unpacked application for the current OS and architecture. On macOS, this local
directory build explicitly uses electron-builder's ad-hoc signing for the entire
bundle, including its nested helpers. The post-sign hook verifies the bundle with
`codesign --verify --deep --strict` and checks its signing identifier. A missing or
invalid signature fails packaging; resources must not be changed after signing.
Other platforms do not receive the macOS signing override.

`pnpm run package` creates the configured distributable and does not force ad-hoc
signing. macOS distribution requires a configured signing certificate (for example,
Developer ID through electron-builder's signing configuration); missing signing
credentials fail the build. Public distribution also requires the appropriate
platform notarization configuration. Local ad-hoc signatures are not stable
developer identities: a rebuilt app can require renewed macOS permissions. Use a
stable Developer ID for distributed updates. See the
[electron-builder v26 signing configuration](https://www.electron.build/v26/docs/features/code-signing/code-signing-mac/).
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
executable and run `pnpm run test:e2e:packaged` for the CI smoke test, or
`node tests/e2e/desktop.mjs` for the broader desktop flow.

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

## Browser and computer controls

Open **扩展 → 插件** for the bundled **Chrome**, **独立浏览器** and **电脑操作** cards.
They are optional installations with their own Skill and stdio MCP server;
installing them grants no control permission and starts no process. Desktop
builds bundle their source and a native helper on Windows and macOS. Building the
macOS app requires Xcode Command Line Tools and macOS 14 or later. The Windows
helper uses the inbox .NET Framework compiler and framework assemblies; users
do not need Node.js, Python or a separate .NET SDK.

For **Chrome**, the production entry is **安装 Chrome 扩展**. It prepares the local
Native Messaging host, installs/enables the bundled plugin, then opens its verified
Chrome Web Store listing in Chrome. The user confirms installation in Chrome;
the desktop polls the connection for up to two minutes and also checks on return.
Reconnecting does not reinstall an already enabled bundled plugin or revoke its
conversation approvals. An unpublished release has `chromeWebStoreUrl: null` in
`plugins/chrome-use/distribution.json`, explicitly disables store installation,
and never constructs a fictitious store listing from the development extension ID.
The local unpacked workflow remains under collapsed **开发者选项**. See the Chrome
plugin's publication documentation before setting a real store URL. From the
extension popup, allow agent-created background tabs or share a specific existing
tab; connection alone does not grant either permission. The agent uses the profile's login
state through tab-scoped Chrome APIs; it does not inject OS keyboard events or
require Chrome to be in front. An approved open can start Chrome on macOS or
Windows if it is closed. Windows registers the host under the current user's
Chrome registry key, uses a bundled executable and authenticated named pipe,
and requires neither administrator rights nor a separately installed Node.js.
This release's extension has not yet been published to the Web Store.

For **Browser Use**, click **准备浏览器** to explicitly download matching Chromium,
then **安装**. Subsequent use opens an isolated browser with temporary cookies,
not an existing personal profile. The browser supports Windows, macOS and Linux
(headless on Linux without a graphical session). Page text
and accessibility references support semantic actions. Coordinates additionally
require a current screenshot. Native dialogs, file upload, arbitrary JavaScript
execution and persistent profiles are outside this initial version.

For **Computer Use**, install it on macOS 14+, use **辅助功能** and **屏幕录制** on
its card to request OS permissions, and **检查权限** to refresh status. Follow the
listed component path when diagnosing system grants. The independently launched
**Open Harness Computer Use.app** (`dev.openharness.computeruse`) owns both system
permissions; the main desktop application remains **Open Harness**.
On macOS it is deployed, without changing its signature, to
`userData/computer-use/Open Harness Computer Use.app`, outside the main app bundle
so macOS can attribute Screen Recording to the correct component. Existing
Open Harness/Electron grants are not transferred or removed automatically. A status check
does not trigger a permission prompt, and returning to the app window refreshes
the check automatically. A failed check is reported separately from a denied
permission. If System Settings shows an enabled entry but a previously built
local app has a different signing identity, remove that old entry and add the
current deployed Open Harness Computer Use app shown by the permission card again. After granting permissions, stop the
current control session and try again; the OS may require restarting the host.
On Windows, **电脑操作** uses the bundled **OpenHarnessComputerUse.exe** helper.
Its card reports whether the interactive desktop is available; there are no macOS
permission switches. UI Automation provides window contents and element refs,
PrintWindow captures only the selected window, and native input requires that
window to be in the foreground. An unlocked desktop is required. Higher-integrity
applications and the UAC secure desktop are refused; the helper never requests
administrator elevation. Apps that cannot produce a window-only capture can
still expose accessibility refs, but coordinate actions require a valid capture.

Both executors only control a selected running app and its observed window.
On macOS, Computer Use 0.4 binds a window ID and directs input to that application
without requiring it to be frontmost. Other windows can cover the target;
unrelated animation no longer invalidates an entire observation. The agent must
still verify each result, and some application-specific shortcuts may ignore
background input. The isolated native GUI smoke verifies two fixture apps with
the target behind the foreground app; it does not prove compatibility with every
third-party application.
Native Linux desktop input, secure-field typing and cross-app shortcuts are
not supported. For concurrent background web work, use the Chrome plugin.

At first use, choose **允许**, **始终允许**, or **取消**. The default grant is
for the named application or website origin, shared with the current task's
children. Explicitly selecting the entire capability permits switching targets.
For ordinary MCP, the target is one exact server/tool, resource URI or prompt.
**始终允许** persists the selected target, bound to the trusted server configuration
and plugin content; settings can revoke individual entries. Previously saved
whole-capability grants remain broad and are labelled accordingly. **取消** does
not create a permanent denial. Chrome private tabs still need explicit extension
sharing, and original tab ownership restrictions remain in force. The extension's
shared-tab list is global to the browser profile, not private to one conversation.

**停止控制** cancels current family control and pending requests. It does not erase
saved Always permissions; remove those in settings. Stopping cannot reverse clicks
or typing already delivered. A failed or uncertain action is never automatically
replayed. Browser calls validate actual page origin at every operation; crossing
an unapproved origin requires new consent before observing or manipulating it.
Sensitive password/file inputs cannot be filled through the isolated browser's
ordinary fill action, matching the Chrome/native restrictions.

Enable **此模型支持图片输入** when adding a model that actually supports vision.
This explicit capability is persisted with the model; protocol alone does not
imply vision. Screenshots reach the provider as images; expand a tool result to
load its saved screenshot preview and enlarge it. Without vision, the model receives
structured UI text and a clear image-omission notice. Active model context and
runtime history retain only the latest screenshot group as pixels; older groups
keep content-addressed sidecar references. Live notifications carry references
too, so slow consumers cannot queue full screenshot payloads. Full archive reads
revalidate and restore those pixels. Text history, Wiki and summaries do not receive raw image
bytes or an invented visual description.

See [Chrome](../../plugins/chrome-use/README.md), [Browser Use](../../plugins/browser-use/README.md) and
[Computer Use](../../plugins/computer-use/README.md) for exact tool contracts,
platform limits and standalone setup. From this directory, after preparing the
browser, run:

```sh
pnpm run test:browser
pnpm run test:chrome
pnpm run test:computer
pnpm run test:e2e:computer
```

The browser tests operate real Chromium against local fixtures. The Electron
check exercises plugin setup, scoped grants, revocation, multimodal model
requests and persisted screenshot previews using a local model fixture, without
paid model calls. Rust integration tests separately verify restart and continuation.
For native GUI validation on a prepared macOS session, run from the repository
root:

```sh
node plugins/computer-use/tests/gui-smoke.mjs
cargo test -p agent-core -p agent-runtime -p agent-memory -p agent-providers --offline
```

The native smoke operates only its temporary Cocoa fixture. Missing OS permission
reports `blocked` with exit code 2, which is not a passing GUI test. Windows CI
uses `tests/e2e/windows-controls.mjs` through the installed-app smoke test; its
WinForms fixture is compiled separately and is never shipped in the installer.
The fixture checks actual input effects and cleanup, not just successful tool
responses. Linux has no native desktop executor.

To check permissions for normal use, first run `pnpm run package:dir`, open the
packaged app through Finder or `open`, and check its Computer Use card. Authorize
that app through its permission buttons if needed. `pnpm run test:e2e:native` from
this directory separately starts an app instance with temporary data and launches
the fixture through that app's main process. macOS can attribute a directly
spawned test process to the launching terminal or Codex, so this test alone does
not establish permissions for a Finder-launched app. It saves
`test-results/native-computer-report.json` and the fixture's
screenshot. Set `OPENHARNESS_E2E_EXECUTABLE` to test another packaged executable;
the same override works for `node tests/e2e/computer-use.mjs`. Neither check
requests system permissions automatically.

## Tools and skills

The default project tool surface is `list_files`, `read_file`, `search_files`,
`apply_patch`, `run_command`, `manage_command`, and `load_workspace_dependencies`.
Read-only projects omit `apply_patch` and permit only read-only commands.
Commands are available on macOS and Windows under their OS sandboxes; execution
runs without extra approval within the selected file scope when networking is disabled;
network requests need approval or a matching saved grant. Creating, editing and deleting files
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
audio input attachments and full document layout rendering are not included.
Image-bearing browser/computer tool observations are supported separately as described above.
The attachment picker, drag/drop and file clipboard also extract these document
formats into saved text snapshots. For larger documents, project tools offer
paginated reading and search without adding the entire text to the initial message.

`pnpm run test:e2e:extensions` exercises real Electron IPC, the Rust API, document
fixtures, local Skills and a stateful stdio MCP peer. It covers settings CRUD,
discovery, lazy loading, permissions, approvals, cancellation and restart. MCP
and Skill enable/disable checks also inspect the actual provider tool schema on
the next turn of an existing conversation. MCP crate tests additionally cover
Streamable HTTP JSON/SSE and failure cleanup.

`pnpm run test:e2e:plugins` verifies native directory/ZIP import, package enable and
disable, namespaced Skills, explicit MCP connection checks, update failure
preservation, restart, and uninstall without removing standalone extensions.
It uses temporary packages and a loopback MCP fixture; no external services or
model API calls are required.

`pnpm run test:e2e:memory` uses a loopback model fixture and an isolated desktop
data directory. It verifies automatic Wiki generation, event provenance, read-only
inspection, disable/re-enable behavior, and persistence through unrelated settings
saves and an application restart. No real provider API or user configuration is used.

### Local script runtimes

Real-model chats without a project receive their own persistent **会话文件** directory.
The model writes temporary scripts under `.openharness-tmp/` and links results
under `outputs/`; the composer can open this directory and message links preview
or save generated files. Existing selected project permissions are unchanged.

`run_command` returns after an initial wait (default 10 seconds) with a command ID
if still running. It no longer imposes a 120-second execution deadline. Use
`manage_command` to read output or stop; an optional `timeout_seconds` is a total
deadline. The UI shows running jobs between turns and exposes **查看输出** / **停止**.
These are background processes with captured output, not interactive terminals;
stdin is closed and later input/PTY sessions are not supported. Commands with
`access: "read_only"` may run in parallel, including in child tasks. Their OS
sandbox permits writes only to a private command temporary directory, removed
after exit; it keeps project files read-only. The project remains live while the
command reads it. Only one workspace-write command mutates a workspace at a
time, and it serializes with other workspace mutations. Conflicting write
requests return busy and must be retried after the active write. Root and child
tasks share a limit of eight active commands; pending approvals consume no active slot.
Stopping the task or quitting
stops its processes; history survives restart without replaying commands.

`test:e2e:scripts` verifies this entire flow against a local model fixture and
real interpreters: CSV/image previews, DOCX/PDF generation and readback, shared
Python/Node library imports, pnpm availability, and stopping a job with a child
process. It also checks that no conversation dependency environment is created,
writable temporary files and, on macOS, rejection of writes to the shared runtime.
Set `OPENHARNESS_E2E_EXECUTABLE` to test the packaged executable.

The desktop exposes standalone Node 24.22.1 to `load_workspace_dependencies`.
Its executable is `workspace-runtime/node/bin/node` on macOS/Linux and
`workspace-runtime/node/node.exe` on Windows. It runs independently of Electron;
use the returned absolute path. The runtime includes pnpm, `csv-parse`,
`csv-stringify` and `pngjs`, in addition to Node's standard libraries. The npm CLI
is not shipped. Renderer dependencies and browser plugins are not general script
dependencies. Use the reported pnpm invocation, which includes standalone Node
and the pnpm entry point. Discovery does not add a conversation-specific directory
or store, and a missing bundled Node does not silently fall back to Electron or
the user's system Node. The command environment prepends verified interpreter
directories to `PATH`, so subprocesses can resolve the bundled `node` executable.
CommonJS uses the supplied `NODE_PATH`; ESM can use `createRequire` with one of
the reported module directories. Explicit project dependency work still follows
the project's own toolchain and lockfile.

Desktop builds also include an independent CPython distribution and a modest set
of preinstalled libraries: Pillow, python-docx, python-pptx, openpyxl, XlsxWriter,
pypdf, reportlab, NumPy, pandas and Matplotlib. CSV/JSON and basic statistics can
still use Python's standard library. Choose Python or Node based on the task,
not a blanket language preference. Discovery probes the installed runtime with
`-I -B`, reports actual versions and never downloads or installs packages at app
startup. It does not borrow another application's runtime. System Python is an
explicit fallback through `OPENHARNESS_PYTHON_BINARY` or
`OPENHARNESS_ALLOW_SYSTEM_PYTHON=1`, rather than a silent dependency on the user.

Build-time provisioning uses SHA-256-pinned official Node and
python-build-standalone archives, hash-locked prebuilt PyPI wheels and a pnpm
lockfile. Versions and hashes live in `scripts/workspace-runtime-node.mjs`,
`scripts/workspace-runtime-bundle.mjs` and `scripts/workspace-runtime/`. Downloads
and package caches stay in `resources/workspace-runtime-cache/`; only the staged
`resources/workspace-runtime/` is shipped. Relocation and import probes run before
atomic replacement. Licenses and a provenance notice accompany the runtime.
Bundled Node dependencies use pnpm's hoisted layout with copied package files,
so Windows junctions do not point back into the build cache. Packaging continues
to reject absolute or escaping links instead of relaxing the runtime boundary.

An offline distributor can instead set `OPENHARNESS_PYTHON_RUNTIME_DIR` to an
absolute, relocatable Python payload before building. It must include the listed
libraries, pip and venv, with `bin/python3` on macOS/Linux or `python.exe` on
Windows. The source remains untouched; the build validates the staged copy and
rejects escaping symlinks. Packaging never copies the builder's system Python.
`OPENHARNESS_NODE_RUNTIME_DIR` similarly accepts an absolute, relocatable Node
22+ distribution containing `bin/node` (macOS/Linux) or `node.exe` (Windows),
plus `LICENSE`. Only the independent executable and license are copied; pnpm and
JavaScript libraries come from the lockfile. The build checks version, platform
and architecture after copying. Neither override modifies the supplied source.

All conversations use the same application-managed Python, Node and preinstalled
libraries. The loader only reports that shared inventory: it does not create a
venv, install packages, change package-manager directories or automatically select
a conversation-specific interpreter. The shared Python has an `EXTERNALLY-MANAGED`
marker, and scripts must not modify the bundled libraries or install globally.

Commands start with a cleared environment and retain allowlisted host environment
values, including the user's home and application-data locations. They no longer
allocate a home, pip cache, pnpm store or uv environment for each conversation.
Workspace-write commands use a writable directory inside `.openharness-tmp/`.
Read-only commands instead use a private OS temporary directory and remove it
after process teardown; they do not create scratch files in the project.
`TMPDIR`/`TMP`/`TEMP` and Matplotlib's cache point to writable command scratch
space. These temporary directories hold working files, not copies of the shared
Python/Node installations or per-conversation dependency environments.

Prefer the shared libraries. If extra dependencies are necessary, create or use
an explicit project or temporary environment through an approved command. The
loader continues to report the shared base; invoke the explicitly chosen
environment directly. Project dependency work follows the project's own toolchain
and lockfile. Package-manager cache/store locations follow their normal settings
unless the command explicitly overrides them; those locations do not receive
additional filesystem permissions merely because a package manager uses them.

Directories from the previous `.openharness-tmp/tasks/<stable-id>/` layout are
preserved, but are no longer created or automatically loaded. Existing files and
deliverables under `outputs/` are retained across turns and restarts.

Runtime sharing and temporary directories are not a security sandbox. macOS
commands in the first two presets use Seatbelt; Windows commands use AppContainer plus Job Objects. Both
default to blocked networking. A command can request `network_access: "allowed"`
for explicit approval, while retaining filesystem restrictions. A one-time grant
covers that command and its subprocesses. The user can instead choose
**允许** for subsequent commands within the same scope, shared by the root task
and its children, or **始终允许** to persist that scope for the same conversation
and directory. The permission panel can revoke grants and optionally stop active
work. Session-only grants expire on restart. They never bypass a read-only
project or a child's file ownership. Shared runtimes remain read-only; extra
dependencies must use a writable project or workspace environment.

Automatic review is opt-in and uses an independent, tool-free model request only
for pending command and ordinary MCP actions. Browser/app consent remains manual.
Only validated low-risk, authorized decisions grant a single pending action;
errors, timeouts and uncertain decisions stay pending for the human. The separate
review log reports model usage. It never changes the sandbox or grants Always.

Full Access is a separate, explicit conversation preset: it permits host commands,
networking, file paths outside the working directory, and configured external tools
without repeated approvals. It does not bypass OS permissions, the browser
extension's shared-tab boundary, or explicitly narrowed child-task write leases.
Read-only and network-denied commands still request a sandbox. Changing back to
another preset revokes conversation grants and restores the filesystem boundary.
New conversations do not inherit Full Access. The advanced **收回权限并停止任务**
action also switches Full Access back to **请求批准**.

Windows packages include `openharness-sandbox.exe` next to `api.exe`; the main
process supplies its absolute path via `OPENHARNESS_WINDOWS_SANDBOX_HELPER`.
If the helper is missing or isolation setup fails, execution stops rather than
falling back to ordinary PowerShell. Existing protected objects receive ACL
denials; Windows ACLs do not provide Seatbelt's future-filename pattern rules.
AppContainer Internet/LAN capabilities also do not exempt localhost loopback
isolation. These native behaviors are tested in the Windows CI job; a macOS
cross-compile alone does not establish Windows runtime correctness.

The host stores the discovered runtime manifest outside conversation workspaces
and passes its path to the backend through `OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST`.
Only host-discovered executable/library directories extend the command sandbox's
read access. Runtime discovery does not grant filesystem writes, network access,
or execution permission by itself.
