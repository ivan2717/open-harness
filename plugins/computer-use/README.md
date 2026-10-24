# Computer Use plugin (macOS and Windows)

An importable local plugin with one Skill and one stdio MCP server. On macOS it uses an independent
agent application named **Open Harness Computer Use**. It operates
one explicitly selected running application with macOS Accessibility,
CoreGraphics keyboard input and ScreenCaptureKit APIs. Coordinate clicks and
scrolling additionally use dynamically resolved window-routing APIs described
below. It has no network dependencies and
never reads other applications' windows while observing the selected app.

On Windows the same tools use a native UI Automation helper. Application selection
keeps the existing host approval contract, `bundle_id` plus `pid`; Windows
`bundle_id` is an opaque executable identity. It is not a permission to control
all processes that happen to have the same displayed name.

## Windows build and runtime

Windows 10/11 requires an unlocked interactive desktop. Build the helper on a
Windows machine or CI runner:

```sh
node plugins/computer-use/windows/build.mjs
```

The output is `windows/bin/OpenHarnessComputerUse.exe`. Desktop packaging includes
the helper and a native `launch.exe --mcp` entry point, so installed users do not
need Node, PowerShell scripts, a compiler, or macOS system permission grants.
The host supplies its bundled Node-compatible runtime to the launcher. The JS
bridge directly spawns the helper with `windowsHide: true`, no command shell,
and private stdin/stdout JSON lines. Each MCP session owns one helper process;
release, cancellation and host EOF discard its selection and snapshots.

`computer_status` returns `platform: "windows"`, `desktop_available`,
`capabilities`, `helper_path`, `helper_pid` and `permissions_requested: false`.
It does not invent Accessibility/Screen Recording grants on Windows. The helper
uses Windows UI Automation for observations and control references, and verified
foreground input for typing and navigation. It does not elevate itself, interact
with the secure desktop, or bypass restrictions on elevated applications.
Status readiness is checked before an action; it is not a promise that every
application exposes accessible controls.

Window coordinates on Windows are **physical pixels**, local to the selected
window, with the origin at its top left. The observation states this explicitly.
Screenshot scaling metadata retains `window_width_points` and
`window_height_points` for protocol compatibility; their values are in the
reported coordinate units. Use those values to scale screenshot pixels. A native
desktop action may take foreground focus. For independent browser work that
should leave the user's active tab alone, use Chrome's background-tab plugin.

The Windows installation smoke uses the actual installed helper and MCP launcher,
plus independent WinForms fixture windows. It verifies concrete Unicode input,
click/scroll outcomes, screenshots, consumed snapshots, protected fields and
release/EOF cleanup. The fixture reports its state separately from the helper;
it is not shipped as an end-user capability. A missing interactive desktop fails
the Windows GUI acceptance; mocks and successful compilation are not substitutes.

## macOS build and runtime

Requires macOS 14+, Node.js 20+ and Xcode Command Line Tools **to build**:

```sh
node plugins/computer-use/build.mjs
node plugins/computer-use/build.mjs --output /absolute/staging/directory
node --test plugins/computer-use/tests/server.test.mjs
node --test plugins/computer-use/tests/app-transport.test.mjs
node --test plugins/computer-use/tests/build-cache.test.mjs
```

The builder creates `bin/Open Harness Computer Use.app` with bundle identifier
`dev.openharness.computeruse`, its own sealed signature and `LSUIElement=true`.
It stays out of the Dock and has its own name in the macOS permission list. The
raw `bin/computer-helper` is retained for isolated stdio fixture tests only.
Builds hash native sources, the build script, architecture, Swift toolchain and
SDK versions. Unchanged builds preserve both binaries and the app signature.
The ignored `bin/build-stamp.json` verifies their hashes; changed source or
toolchains rebuild them. A failed compile/sign preserves the previous output.
`--output` builds elsewhere without replacing a currently authorized installation.

Import `plugins/computer-use` in the Open Harness plugin manager after building.
The compiled `.app` is part of the imported immutable package;
source checkout binaries are ignored by Git. Installed packages do not compile,
install dependencies or mutate themselves. Desktop packaging builds/copies the
helper. `launch.sh` uses `OPENHARNESS_NODE_BINARY` when the desktop host supplies
its Electron executable together with `ELECTRON_RUN_AS_NODE=1`; otherwise it uses
`node` on PATH. The host provides the absolute `.app` path through
`OPENHARNESS_COMPUTER_APP`, keeping the installed control component at a stable
app-resource path across plugin updates. With no override, the package's own
`bin/Open Harness Computer Use.app` is used. `OPENHARNESS_COMPUTER_HELPER` is an
explicit compatibility override for stdio tests; the app setting takes precedence.
Paths are host configuration and never come from model arguments. A missing app
fails rather than silently reverting to execution under the desktop host identity.

The bridge launches the app through **LaunchServices** (`open -g -j -n -W`), not
by spawning its executable. The app registers its own bundle URL through public
LaunchServices APIs so System Settings can resolve its name. Each MCP session has a private Unix socket in a
0700 directory, a 0600 socket/token file, and a random 256-bit handshake token.
The native peer checks ownership, file types, permissions and the socket peer UID;
the bridge checks the nonce, protocol and bundle ID before forwarding requests.
The secret is never put in argv or model output. The existing newline JSON
protocol then runs over that socket. Host disconnect/cancellation exits the
independent app, including during a pending operation. No TCP listener is opened.

The Skill is guidance, not a permission grant. The host approves app selection;
the independent control app also requires macOS Accessibility permission. Window
screenshots need Screen Recording. `computer_status` reports both without prompting.
Grant them to **Open Harness Computer Use** in System Settings. Existing grants to
Open Harness/Electron are not silently transferred or removed. No TCC database
edits or automatic permission
requests are performed by the model or status checks. The native helper has two
host-only methods, `request_accessibility` and `request_screen_recording`, for an
explicit user click in the desktop settings UI. These use the public macOS
permission APIs and are deliberately absent from the MCP tool catalog. Their
response reports `{ permission, granted, permissions_requested: true, helper_path,
settings_url }`; the UI should recheck status afterwards because grants may
require reopening the control session. The desktop host calls
`node host.mjs --request status|request_accessibility|request_screen_recording`;
the proxy returns one `{id:1,result}` or `{id:1,error}` JSON line and uses the same
LaunchServices bridge. Status additionally reports `permission_app_name` and
`permission_bundle_id`. Tests never call the prompting methods.

A stable bundle name/ID alone does not make ad-hoc signatures survive actual code
changes. Development builds use ad-hoc signing; release distribution should use a
stable signing certificate. This implementation never resets permissions, weakens
a designated requirement or changes a user's existing authorized app automatically.

## Tools and state

- `computer_status`: permission/selection status, without app contents.
- `computer_apps`: regular running app names, bundle IDs and PIDs only.
- `computer_select_app`: exact bundle ID + PID, activates the app; no fuzzy match
  or app launch. Stores executable path + launch time to reject PID reuse.
- `computer_observe`: bounded focused-window AX tree, optional window-only PNG.
- `computer_click`: AX ref, or a point derived from a current screenshot.
- `computer_type`: up to 2,000 UTF-16 units; direct Unicode events, no clipboard.
- `computer_key`: a bounded set of navigation/editing keys.
- `computer_scroll`: scroll inside the selected window.
- `computer_release`: clear selection and terminate the helper.

An observation supplies a random snapshot ID, valid for one action and at most
60 seconds. Before action dispatch, the helper rechecks process identity,
foreground app, window identity/bounds, focused element and the observed AX tree.
The native helper keeps AppKit's main run loop running while stdin is read on a
background queue. Foreground checks query the system Accessibility focused-app
PID directly, so cached NSWorkspace state cannot authorize input after focus moves.
Referenced AX elements must remain identical. Coordinate actions additionally
require a matching fresh screenshot and a hit-test owned by the selected app.
Coordinates are window-local **points**, not screenshot pixels; image dimensions
and point dimensions are returned for conversion. Animating windows may require
AX actions rather than coordinates because changed pixels invalidate screenshots.

Coordinate delivery uses the window-directed macOS routing approach used by
DeepSeek Harness: `CGEventSetWindowLocation`, `SLEventSetIntegerValueField` and
`SLEventPostToPid`, dynamically resolved from SkyLight. Public PID-only mouse
events were tested but arrived with a missing window number; passing a public
NSEvent window number also lost its local coordinates during conversion. The
window-routing path targets both the verified PID and exact window. It sends each
action once and **never falls back to global input**. These are private macOS
interfaces, so OS updates can make coordinate input unavailable; missing symbols
return `POINTER_ROUTING_UNAVAILABLE`, while AX clicks remain available.

Actions return `dispatched: true, verified: false`. The model must observe and
verify the result. No input is automatically retried. A timeout, protocol error,
cancellation or release terminates the helper and invalidates queued operations;
subsequent work must select an app again. Cancellation cannot undo input already
posted to the OS. Every server session has one serialized helper; the host owns
cross-session authorization and exclusion for shared targets.

AX observations are limited to 250 nodes, depth 16 and six seconds, with bounded
text and a truncation flag. Secure text values are omitted and typing into secure
fields is rejected. Screenshots use a maximum 1,600-pixel edge and 4 MiB PNG.
The escaped MCP text envelope is bounded to 28 KiB. Oversized observations shorten
labels first, then omit static/container nodes before actionable controls. Snapshot
IDs, retained refs and coordinate metadata stay intact; the native snapshot used
for validation is unchanged. A truncation marker makes omitted content explicit.
Observations are untrusted data. Existing OS permissions do not authorize the
agent to send messages, spend money or perform unrelated actions.

## Verification

`server.test.mjs` verifies actual stdio MCP initialization, native permission
status, missing app rejection, argument validation, ordering, session retention,
cancellation, timeout, release, corrupted response and no replay after failures.
Build the helper first; the native check is skipped only off macOS.

```sh
node plugins/computer-use/tests/gui-smoke.mjs --build-only
node plugins/computer-use/tests/gui-smoke.mjs
```

The GUI smoke uses `OPENHARNESS_COMPUTER_APP` first and defaults to the source
package's `bin/Open Harness Computer Use.app`. It verifies the independent bundle
identity as well as the selected app's input results. An explicit
`OPENHARNESS_COMPUTER_HELPER` remains available for isolated stdio fixtures only.
Results include `helper_path`; granting a different executable permissions is not
proof that the independent control app is authorized.

For the packaged independent control app with its own macOS grants:

```sh
cd apps/desktop
node tests/e2e/native-computer.mjs
```

This launches an isolated packaged app instance and runs the smoke as its child.
It saves the actual fixture screenshot and JSON report in `test-results/`.
This legacy test exercises the raw helper, not the new independent-app permission
identity. Use the production `host.mjs --request status` bridge to verify the latter;
status is read-only and never prompts. The report includes observed text, click count and scroll offset, not just event
dispatch. It neither requests permissions nor uses production app data.

The independent app has a separate opt-in launch/identity/exit test:

```sh
OPENHARNESS_COMPUTER_TEST_APP='/absolute/path/Open Harness Computer Use.app' \
  node --test plugins/computer-use/tests/app-status.test.mjs
```

It checks real LaunchServices startup, the independent bundle identity and process
exit after channel closure. It does not require already granted permissions and
does not claim that false permission states are authorized.

The GUI smoke creates its own temporary Cocoa app and only operates its instances.
It checks Unicode typing, AX press, PNG capture, coordinate click, scrolling,
single-use snapshots, stale-window rejection and rejection after focus moves to
another fixture instance, against actual window state.
It does not inspect or control user apps. Missing system permissions return a
structured `blocked` result with exit code 2; this is **not** a passing GUI test.
The same blocked result is reported when `com.apple.loginwindow` is foreground;
the test must be rerun after the user unlocks the desktop.
The fixture and helper are cleaned up afterwards. Do not enable global computer
permissions merely to make a CI test pass; run the GUI test in an explicitly
prepared local session.

The plugin does not support Linux, background desktop input,
password entry, drag gestures, menus outside the selected window or cross-app
shortcuts. Application-specific AX quality varies; an unavailable control is
reported rather than guessed.
