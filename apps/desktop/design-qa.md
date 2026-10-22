# Composer design QA

Date: 2026-10-22

## Initial composer target and scope

Adapt the supplied Codex composer's grouping and interactions to Open Harness.
Keep the existing light palette, system fonts, Lucide icons and real task behavior.
The user requested borrowing the design, rather than a dark-theme pixel clone.
The follow-up removes duplicate entry points: project and permissions belong
together, model management belongs to the model picker, and global settings stay
in the sidebar. The composer has no generic plus menu or settings icon.
Voice, attachments, remote execution and changing a running conversation's model
are outside scope; no nonfunctional controls were added for them.
Later attachment and rich-message work is recorded in the follow-ups below.

## Visual evidence and normalization

- Source visual truth: `/var/folders/rv/ghrq1zqn405dtldv_2vymry80000gn/T/codex-clipboard-b8093edb-aa57-42ec-a5fb-7101b5844809.png`
- Previous UI: `/var/folders/rv/ghrq1zqn405dtldv_2vymry80000gn/T/codex-clipboard-89f28f08-5c42-4b7a-bb48-e7e9cbf68e96.png`
- Follow-up feedback: `/var/folders/rv/ghrq1zqn405dtldv_2vymry80000gn/T/codex-clipboard-033832d7-021c-41c9-9e7f-70d1e74ea530.png`
- Focused implementation: `test-results/desktop-composer-reference.png`
- Full view: `test-results/desktop-start.png`, `test-results/desktop-workspace-entry-ready.png`
- Menus: `test-results/desktop-composer-project-menu.png`, `test-results/desktop-composer-permission-menu.png`, `test-results/desktop-composer-model-menu.png`
- Minimum window: `test-results/desktop-composer-compact.png`
- Context controls: `test-results/desktop-context-controls.png`

Source and focused implementation were opened together in one comparison input;
the full view and each menu state were also inspected. The source is 1766 × 518 px
with unknown capture density; its visible composer occupies roughly 1472 × 272 px.
The implementation crop shows the full composer at DPR 2. Full views
are 1440 × 940 CSS px / 2880 × 1880 pixels; the minimum window is 1000 × 680 CSS px /
2000 × 1360 pixels. Comparison aligns composer regions by relative scale, excluding
surrounding conversation chrome. Pixel fidelity across unknown source density,
different themes and fixture text is not claimed. The focused crop makes footer
alignment and labels readable; full views establish overall hierarchy and fit.

## Findings

No actionable P0/P1/P2 differences remain within the requested adaptation.

- **Fonts and typography:** existing system stack; 14 px task text, 24 px line
  height, 12 px controls. Long project/model names truncate, preserving action space.
- **Spacing and layout:** inset project/environment strip behind the rounded editor;
  project permissions appear beside the selected project. Only the model and round
  send/stop controls occupy the editor footer. Editor grows from 76 to 220 px.
- **Colors and tokens:** intentional retention of the pale green/white palette;
  context surface, focused input, menu selection and disabled/enabled send remain
  distinguishable. Dark theme is an accepted product difference.
- **Image quality and icons:** ordinary UI icons only; existing Lucide folder,
  laptop, shield, check, arrow and stop. No raster assets are needed.
- **Copy/content:** labels reflect supported functions: 此计算机, 只读,
  允许修改, 添加模型 and 添加／管理模型. No permission label appears before a project is
  selected. Permission details appear in the menu. Existing runs show
  their fixed project, permissions and model as text with explanatory tooltips.
- **Interaction and responsiveness:** menu selected/focus/hover states, arrow keys,
  Escape focus return, outside-click dismissal and menu exclusivity passed.
  Popups use a portal to avoid editor clipping and fit the tested minimum window.
- **Single-purpose entry points:** project menu contains change/remove; model menu
  contains model management. Run details and project results remain in the header.
  Manual compaction/cancellation is in the inspector's context tab, alongside its
  budget and checkpoints. Global settings remain in the sidebar.

## Functional verification

Three actual Electron E2E suites passed with local fixture models and isolated
data, without paid provider calls. Coverage includes empty/configured models,
legacy demo preferences, project grants and permissions, preserved drafts,
automatic textarea sizing, menu keyboard interactions, command approvals,
file review/undo, context compaction/cancellation and recovery. The suites checked
uncaught renderer errors. Type checking, 21 unit tests, formatting and build passed.
After the entry-point cleanup, all three E2E suites, type checking, formatting and
build passed again. Updated coverage verifies the absence of duplicate composer
controls, project removal and reselection restoring read-only access, preserved
drafts, and compaction/cancellation through the context tab. No new unit tests were
needed for these UI changes.

Independent review identified a delayed-creation navigation race. Creation now
keeps its pending state across pages, only clears the submitted project selection,
and does not navigate after the submitting page unmounts. The workspace suite was
rerun successfully with two delayed real IPC responses, covering duplicate-submit
prevention, navigation preservation, unchanged draft cleanup and edited draft
retention. The updated macOS application was packaged and reopened; its new
composer controls and three existing conversations were verified in the native UI.

## Comparison history

The initial grouping was revised after the user's duplicate-entry feedback.
The updated empty state, selected project, project/permission/model menus, minimum
window and context controls were visually inspected. No actionable visual issues
remain in this scope. Theme, fixture text and unsupported Codex capabilities are
intentional differences.

## Implementation checklist

- [x] Group project, permissions, models and task actions around the editor.
- [x] Preserve native directory grants and per-run execution bindings.
- [x] Check empty/configured states, menu states and minimum desktop size.
- [x] Exercise task flows and keyboard interactions in actual Electron.
- [x] Inspect full-view and focused evidence with the reference.

## Follow-up polish

No blocking follow-up; additional themes or capabilities require their own scope.

final result: passed

## Automatic model settings follow-up

The model form no longer requires users to choose token counts. Context capacity
and the per-request output budget live in a collapsed “高级设置” disclosure. Blank
fields use automatic settings; manual values can be cleared with “恢复自动设置”.
Changing the endpoint, model or protocol clears overrides from the previous model.
Provider metadata is retained while editing or restoring the same model's limits.

Inspected evidence: `test-results/desktop-model-settings.png` (collapsed),
`test-results/desktop-model-advanced.png` (manual values), and
`test-results/desktop-model-auto.png` (metadata-derived placeholders). The disclosure,
labels, reset control and save action fit the existing settings layout. No blocking
visual issues remain in this scope.

Type checking, 32 unit tests, formatting, build and all three Electron E2E suites
passed. Five model-capacity scenarios cover conservative fallback, manual override,
metadata and restoring automatic values, metadata failure, and Messages metadata.
The tests verify the resulting runtime context and outgoing output limit, then
restart and verify the same model identities and capacities without new discovery
requests. Official model previews are tested without saving or sending requests;
all network fixtures are local, with no paid provider calls.

The macOS app was repackaged and reopened. Native UI verification confirmed the
collapsed automatic settings, expanded optional fields and reset control. The
four existing conversations remain present; the app is left on the settings page
with advanced settings collapsed.

## Chronological timeline follow-up

Timeline events now follow their persisted sequence from top to bottom, beginning
with the run start and appending newer events below. The filled marker identifies
the final, latest event. The existing desktop E2E suite passed and its refreshed
`test-results/desktop-timeline.png` was inspected for chronological layout. Type
checking, build and formatting passed.

## File references follow-up

The composer accepts explicit local text files through a plus button, drag
and drop, or a file-manager copy/paste. A filename-and-size chip appears above the
draft, with a remove action; the conversation shows the same concise metadata.
The drag target gets a temporary outline and drop hint. Ordinary text and web
links keep native paste behavior. Existing project/model controls remain in place.

Inspected `test-results/desktop-file-references-draft.png` and
`test-results/desktop-file-references-conversation.png`; no clipped controls or
unexpected expanded file contents were found. The new Electron E2E exercises real
disk-backed File objects and the native macOS clipboard shortcut, removal,
deduplication, navigation, admission failure, file-only messages, create/input/resume,
and restart. The local provider fixture receives all three saved snapshots even
after source files change. The test restores the original OS clipboard on exit.

All four Electron suites, 42 desktop unit tests, core/runtime regression tests and
API tests passed. Workspace all-target compilation, TypeScript, formatting and
release build passed. File contents participate in context measurements and
compression. Unsupported binary formats and oversized files produce explicit
errors; references do not expand project access permissions.
The macOS application was repackaged and reopened; native UI inspection confirmed
the new attachment control and preservation of all five existing conversations.

The attachment button now uses a circular plus with a custom “添加文件” tooltip
that also explains drag-and-drop and the platform's paste shortcut. The tooltip
appears on hover or keyboard focus and is rendered outside the composer's clipped
container. Native macOS inspection verified the visible tooltip, Escape dismissal,
and opening/cancelling the file picker. Type checking and the desktop build passed;
the packaged app was updated and reopened.

## Rich messages and output resources follow-up

Assistant messages now render Markdown/GFM with heading hierarchy, code highlighting
and copying, tables, lists and quotes. Input text remains literal. Linked local
images enlarge in a dialog; audio/video use native playback controls. Text files
preview in-app, and file actions offer open, reveal and save as appropriate.
External media requires an explicit load action. PDF/Office documents use system
applications; input attachments remain UTF-8 text snapshots.

Inspected `test-results/rich-messages.png` and the image preview dialog. No clipped
content or nested interactive controls were found. The new Electron E2E covers
partial streaming and final Markdown, code copying through IPC, PNG/SVG rendering,
actual WAV/WebM playback, byte ranges, remote-load opt-in, text preview, native file
action routing and saving, rejected paths, and snapshots after deleting source
files and restarting. The SVG check confirms scripts and nested external loads
remain inactive. File action dialogs and external launches are mocked in tests.

All 71 desktop unit tests, TypeScript, formatting and production build passed.
The baseline desktop and attachment Electron suites passed, as did the API desktop
and workspace integration suites. The updated macOS application was packaged and
reopened: all six existing conversations remain, the user's original “烛影” answer
now has a rendered heading, and the existing “李秀成.md” attachment opens its saved
text preview. No new provider request was sent during these checks.

## Quiet conversation activity follow-up

Conversation lists and headers now show only a small rotating loader while active
and a static green dot while idle. Visible waiting/completed labels, the duplicated
header badge and the completed-result footer are removed. The composer uses the
same neutral border and prompt in every idle state, with no waiting-for-input
banner. Errors, command approvals, cancellation and interruption recovery remain
available in their relevant controls.

Manual compaction activity is included in the API's run summary and sidebar cache,
so it also shows the loader and returns to a dot after completion or cancellation.
The detailed run JSON remains compatible; runtime statuses still determine which
input/resume operation to use.

Inspected `test-results/desktop-activity-running.png` and
`test-results/desktop-activity-idle.png`. Desktop, model and file-reference Electron
suites passed, covering active/idle transitions, continued messages, cancellation,
manual compaction and file-only follow-ups. The desktop suite checks that the
loader is animated. All 72 desktop unit tests and 23 API tests, TypeScript,
formatting and production builds passed.
The packaged macOS app was reopened and the original story conversation inspected:
all six conversations remain, idle dots are green, and neither the header nor the
composer displays the removed waiting labels.

## Document tools, MCP and Skills

Added compact Settings sections for MCP services and Skills, with form-based
stdio/HTTP configuration, connection testing, editing, enable switches and
removal. Skill installation uses an explicit folder entry and refresh. A small
conversation disclosure shows project and personal Skills; pending MCP calls
show actionable approval cards with inspectable arguments.

Inspected `test-results/desktop-extensions-settings.png` and
`test-results/desktop-mcp-approval.png`. The Electron extension suite passed actual
configuration CRUD, Skill switches/refresh, read-only DOCX/PDF extraction, lazy
Skill/resource reads, stateful MCP calls, resources/prompts, approvals, denial,
cancellation and restart persistence. The existing workspace and three-protocol
model suites also passed. The model fixture now reports usage proportional to its
actual request instead of a fixed value that masked growing tool definitions.

Rust workspace: 400 tests passed; strict workspace Clippy passed. Desktop: 77
unit tests, TypeScript, production build and formatting passed. The actual files
from the reported failure were checked locally without exposing their text or
calling a model: Word extracted 111 source units, PDF extracted all five pages.
No source document was modified.

The packaged macOS application was fully restarted and inspected. All seven
existing conversations loaded, the configured model remained available, and
Settings displayed the new tool-service and skill controls. MCP connection tests
have a dedicated 70-second desktop deadline covering the service's 60-second
deadline; ordinary API requests retain their 30-second limit.
