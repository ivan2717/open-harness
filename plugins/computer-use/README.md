# Computer Use plugin (macOS and Windows)

An importable local plugin with one Skill and one stdio MCP server. On macOS it uses an independent
agent application named **Open Harness Computer Use**. Each session binds one
explicitly selected running application and window. On macOS, accessibility
operations and window-directed input can operate that window in the background:
selection does not activate the app, and event delivery does not use global input
or move the user's pointer. ScreenCaptureKit supplies window-only screenshots,
including covered windows. The private window-routing APIs and compatibility
limits are described below. The plugin has no network dependencies and observes
contents only inside the selected app's bound window.

On Windows the same tools use a native UI Automation helper. Application selection
keeps the existing host approval contract, `bundle_id` plus `pid`; Windows
`bundle_id` is an opaque executable identity. It is not a permission to control
all processes that happen to have the same displayed name.
Its Windows form is `win32:<process name>:<executable path hash>` so approval can
show the application name. The name is only a display hint: authorization still
matches the complete returned identity and PID, and the helper rechecks the
executable path and process creation time before acting.

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
reported coordinate units. Use those values to scale screenshot pixels.
`computer_select_app` activates the selected Windows window; subsequent input
requires it to remain foreground and never takes focus back automatically.
Windows status explicitly reports `capabilities.background_input: false`.
For independent browser work, use Chrome's background-tab plugin.

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
`OPENHARNESS_COMPUTER_APP`. On macOS the desktop copies the signed component
unchanged into `userData/computer-use/Open Harness Computer Use.app`, outside
the main `.app` bundle. This stable location prevents Screen Recording from
being attributed to the enclosing desktop app. Unchanged components are reused;
deployment failure is reported without falling back to a different permission
identity. With no override, the package's own
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

The Skill is guidance, not a permission grant. The host approves the displayed
application identity. Allow covers that application for the conversation and
collaborative tasks; Always allow persists the same application, bound to the
plugin and server configuration. Process IDs remain live target checks rather
than reusable permission identities. Another application requires its own grant
unless the user explicitly chooses the entire computer-use capability. Cancel
does not grant access; permissions can be inspected and revoked in settings.
The host independently validates each app selection;
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

- `computer_status`: permission, selection and platform capabilities, without app contents.
- `computer_apps`: running app names, exact application identities and PIDs.
- `computer_select_app`: exact `bundle_id` + `pid`, optionally an integer
  `window_id` returned by `computer_windows`. It never launches an app.
- `computer_windows`: window IDs, titles and bounds for the selected app only.
- `computer_observe`: the bound window's AX/UIA tree and an optional window-only PNG.
- `computer_click`: a current accessibility `ref`, or window-local `x` and `y`.
  Optional `button` is `left`, `right` or `middle`; `click_count` is 1–3;
  `modifiers` contains distinct `shift`, `control`, `alt` or `meta` values.
- `computer_type`: literal Unicode, up to 2,000 UTF-16 units, without a clipboard.
- `computer_key`: a key or application chord such as `control+a`, `meta+shift+z`
  or `shift+tab`. Letters, digits, F1–F12 and navigation/editing keys are supported;
  `ctrl`, `cmd`, `command` and `option` are modifier aliases. System/app-switching
  shortcuts are rejected. Windows also rejects logo-key (`meta`) combinations.
- `computer_scroll`: scroll at a screenshot-derived point inside the bound window;
  positive `delta_y` scrolls up and negative scrolls down.
- `computer_drag`: one left-button gesture from `from_x`, `from_y` to `to_x`,
  `to_y`, with optional modifiers.
- `computer_set_value`: assign a text value to a ref observed with
  `settable_value: true`; an empty value clears it. Maximum 2,000 UTF-16 units.
- `computer_action`: perform an exact name from that ref's observed `actions`
  array. The native helper checks both the observed and current capability.
- `computer_release`: clear selection and terminate the helper without closing the app.

App selection checks executable and process birth identity to reject PID reuse.
Window selection binds an exact native window identity; observation does not
follow another focused window or a newly opened dialog. Use `computer_windows`
and select an explicit ID when changing windows. If the helper cannot choose a unique
initial window, selection succeeds with `selected_window_id: null` and retains
the app identity for `computer_windows`; choose one of its returned IDs and call
`computer_select_app` again. An explicitly requested missing or foreign ID fails.

| Behavior | macOS | Windows |
| --- | --- | --- |
| Selection | Binds the window without activating its app | Activates the selected HWND |
| Input | AX or events directed to the bound process/window | UIA or foreground input, including SendInput |
| Coordinates | Window-local points | Window-local physical pixels |
| Other app becomes foreground | Background operations remain available when the bound target is valid | Input is rejected until the intended window is selected/foreground again |
| Keyboard focus | Validates the observed control; visual canvas fallback requires a screenshot in the current observation and unambiguous app-local window | Validates the observed control and selected foreground window |

macOS discovers windows through WindowServer, retaining the exact window ID and
owner process birth identity. It includes verified helper processes inside the
approved app bundle. AXWindows, AXFocusedWindow and AXMainWindow enrich these
windows when available; AX failures or mismatched AX geometry disable semantic
refs without blocking window screenshots. Observation reports `accessibility`
as `available`, `partial` or `unavailable`; the last state automatically includes
a screenshot. An empty AX tree is not treated as a missing app or missing system
permission. AX controls can recover on the next observation without reselection.
Covered windows remain controllable without AX. Off-screen windows need a live
AX window: macOS can retain closed window IDs and stale image buffers, which must
not be treated as a still-open window. This does not require the app to be foreground.

Canvas keyboard input with no observed AX focus requires a current screenshot
and either a matching app-local focused window or a sole on-screen window owned
by the target process (covered windows count as on-screen). Multiple on-screen windows with unknown focus reject keyboard
input; the helper never guesses a destination or changes focus. Known protected
controls and macOS secure input remain blocked. A changed observed text control
does not fall back to unverified canvas input.

Every observation supplies a random snapshot ID and
`snapshot_policy: "single_action_until_replaced"`. There is no fixed age limit:
model latency alone does not invalidate an observation. A new observation, target
selection or session shutdown replaces it; each action consumes it before native
validation, including failed or uncertain actions. Pointer gestures require a
screenshot in that observation. Nondefault button/count/
modifier options on a ref are pointer gestures and also require a screenshot.
Default ref clicks use an advertised semantic action; an unsupported semantic
click does not automatically fall back to coordinates.

Actions recheck process and window identity and the relevant ref or focused
control. A window resize rejects the snapshot. Moving the same window updates
its screen translation while preserving window-local coordinates. The helper
no longer requires the entire accessibility tree or every screenshot pixel to
remain identical: unrelated animations and changing labels elsewhere do not
invalidate a target. Coordinate input checks an observed accessible target's
identity and local bounds when one is present. Windows also checks the current
UIA hit target. Before pointer dispatch, both platforms capture the bound window
again and compare only a 21×21 screenshot-pixel patch around each target (both
endpoints for a drag). A per-channel difference over 24 affecting more than 10%
of the patch returns `VISUAL_TARGET_CHANGED` with `dispatched: false`; capture
failure also prevents dispatch. Minor rendering noise and animation elsewhere
are tolerated. This local check does not detect every semantic change or changes
after validation, and keyboard input still uses window/focus checks rather than
pixel comparison. Semantic ref actions do not require an extra screenshot. A screenshot is evidence
of the observed UI, not a promise that its controls cannot change before input;
use current refs when available and observe again after any unexpected change.

Scale image coordinates with `window_width_points / screenshot.width` and
`window_height_points / screenshot.height`. The compatibility field names retain
`points` on Windows, but their units are the `coordinate_space` reported by that
observation. Never pass resized screenshot pixels directly as window coordinates.

macOS event delivery uses `CGEventSetWindowLocation`, `SLEventSetIntegerValueField`
and `SLEventPostToPid`, dynamically resolved from SkyLight. The events identify
both the verified owner PID and exact window. Keyboard input checks the bound
window's app-local focus rather than the system foreground app. No global input,
app activation or pointer-warp fallback is used. These private interfaces can
change between macOS versions, and some applications may ignore background
input. Missing routing symbols return `INPUT_ROUTING_UNAVAILABLE`; accessible
controls can still support direct AX actions or value assignment. Always verify
the result in the app rather than treating delivery as success.

For an observed editable text field, macOS `meta+a` uses its writable
`AXSelectedTextRange` and checks the selection by reading it back. Other shortcuts
use directed key events and may depend on the application's background support.
Cancellation/EOF stops further input and gives an in-flight pointer gesture a
bounded opportunity to send its matching release to the original window; process
crashes or SIGKILL cannot guarantee cleanup.

Successful input normally returns `dispatched: true`, `verified: false`, and
`verification.status: "not_verified"`. A macOS value assignment or text selection can report
`verified: true` with `accessibility_readback_matched`; this confirms the control's value or selection,
not a later save, submission or other workflow outcome. Observe and check the
specific intended result before continuing. No side-effecting action is retried
automatically, including after an AX timeout or a failed provider acknowledgement.

Errors expose `code`, `recoverable`, `dispatched` and `next`. Only an explicit
`dispatched: false` establishes that this failed operation sent no input: refresh
state and make a new decision before any further action. `dispatched: null`, a
missing delivery state, a timeout or a lost connection means delivery is unknown.
Do not replay the operation; inspect whether it already happened. `recoverable`
is recovery guidance, not permission to repeat a click, send, payment or text batch.
If the outcome cannot be established, stop that operation instead of guessing.

Timeout, protocol failure, cancellation and release stop the helper and invalidate
queued operations. Subsequent work must select the app/window again. Cancellation
cannot undo input already posted to the OS. Each MCP session has one serialized
helper; the host owns cross-session authorization and exclusion for shared targets.

Observations are bounded to 250 nodes, depth 16 and a bounded traversal time, with
bounded text and a truncation flag. Secure text values are omitted and protected
input is unsupported. Screenshots have a maximum 1,600-pixel edge and 4 MiB PNG.
The escaped MCP text envelope is bounded to 28 KiB. Oversized observations shorten
labels before omitting static/container nodes; focused and actionable controls
receive priority. Snapshot IDs, retained refs, action names, selected window IDs
and coordinate metadata remain intact. Truncation does not mutate native snapshot
state, and omitted content is not evidence that a control is absent. Observations
and window titles are untrusted task data. OS or app grants do not authorize
sending messages, spending money or performing unrelated actions.

## Verification

The Node tests cover tool schemas and key chords, actual stdio MCP initialization,
native permission status, missing app rejection, ordering, session retention,
cancellation, timeout, release, corrupted responses, structured delivery errors,
metadata budgets and no replay after failures. Build the helper first; the native
MCP permission check is skipped off macOS.

```sh
node --test plugins/computer-use/tests/*.test.mjs
```

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
It uses the installed independent component through `OPENHARNESS_COMPUTER_APP`.
The report checks fixture outcomes rather than only event dispatch. The production
`host.mjs --request status` bridge can separately check that component's identity
and grants; status is read-only and never prompts. Tests neither request
permissions nor use production app data.

The independent app has a separate opt-in launch/identity/exit test:

```sh
OPENHARNESS_COMPUTER_TEST_APP='/absolute/path/Open Harness Computer Use.app' \
  node --test plugins/computer-use/tests/app-status.test.mjs
```

It checks real LaunchServices startup, the independent bundle identity and process
exit after channel closure. It does not require already granted permissions and
does not claim that false permission states are authorized.

The GUI smoke creates its own temporary Cocoa app and only operates its instances.
It keeps fixture B foreground while operating background fixture A, checking
that B stays active and A never activates. The smoke covers Unicode typing,
combination keys, advertised AX actions, writable values, protected-field refusal,
covered-window screenshots, animated content, right/middle/multiple clicks,
modifiers, drag, scroll and independent foreground responder input. It also checks
single-use snapshots, translated/resized windows, empty AXWindows, absent AX
windows/focus, visual canvas input, recovered AX, same-app focus changes and
closed-window rejection against the fixtures' own reported state.
It does not inspect or control user apps. Missing system permissions return a
structured `blocked` result with exit code 2; this is **not** a passing GUI test.
The same blocked result is reported when `com.apple.loginwindow` is foreground;
the test must be rerun after the user unlocks the desktop.
The fixture and helper are cleaned up afterwards. Do not enable global computer
permissions merely to make a CI test pass; run the GUI test in an explicitly
prepared local session.

The plugin does not support Linux, password entry, system permission dialogs,
menus outside the selected window, cross-app shortcuts or app launching. Windows
background input is unsupported. macOS background compatibility depends on the
application and OS routing APIs; successful compilation or permission status does
not establish end-to-end input support. An unavailable control is reported rather
than guessed.
