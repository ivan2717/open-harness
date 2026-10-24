# Windows native Computer Use

The helper targets Windows 10 version 1703 or later and x64. It uses the Windows
.NET Framework 4.x runtime and managed UI Automation; end users do not install
Node.js or a separate .NET SDK. The desktop starts the helper with redirected
stdio and no console window.

## Build and verify

On Windows, run `node plugins/computer-use/windows/build.mjs`. The compiler is the
inbox `Microsoft.NET/Framework64/v4.0.30319/csc.exe`. References come from that
framework directory and its `WPF` subdirectory. The result is
`windows/bin/OpenHarnessComputerUse.exe`. Every build runs the helper's read-only
`--self-test`, including native input structure layout and protocol validation.
`--output <absolute-directory>` selects another destination.

`--fixture` additionally builds `ComputerUseFixture.exe` for CI; it is not part of
the normal production build. The fixture accepts `--state <absolute-json-path>`,
`--password-focus`, and `--medium-integrity`. The last flag lowers only the test
process's own token before creating its window, so an elevated CI runner can
test the production prohibition on elevated target applications. The fixture
retains its PID and verifies the resulting token. Its state file contains text,
click count, scroll position, password **length only**, PID and readiness. UIA
names are `Task input`, `Apply`, `Password` and `Scroll area`; automation IDs can
differ between Windows accessibility compatibility modes.

## Protocol and boundaries

With no arguments, the helper reads bounded UTF-8 JSON lines of
`{ "id": 1, "method": "status", "params": {} }` and writes
`{ "id": 1, "result": {} }` or `{ "id": 1, "error": { "code": "...", "message": "..." } }`.
Methods are `status`, `apps`, `select_app`, `observe`, `click`, `type`, `key`, and
`scroll`. Status reports actual interactive desktop availability, Windows
capabilities, helper PID and path; Windows has no macOS permission prompts.
Keep stdin open while awaiting a response. EOF terminates the process even when
a target UI Automation provider or capture call hangs. Each request also has a
10-second hard deadline, after which the helper reports a timeout and exits.

Application IDs use `win32:<process-name>:<canonical-path-hash>`. The bounded
process name makes approval readable, while the hash binds the canonical
executable path. Selection additionally binds the PID, process creation time,
session, top-level HWND, owning thread and UIA runtime identity. An HWND is not
implicitly replaced if a dialog or another window appears. Input requires the
exact selected foreground window and unchanged focus; it consumes a snapshot
valid for 60 seconds. Ref actions address verified elements. Coordinate actions
require unchanged window-only pixels and a hit test inside the selected window.
All coordinates are physical pixels relative to the window's top-left corner,
not the pixels of the potentially resized screenshot.

Scrolling uses a vertically scrollable UIA target or ancestor without changing
focus. UIA supplies small/large semantic increments, so the magnitude is not an
exact pixel distance. Traditional controls without ScrollPattern receive one
bounded `WM_MOUSEWHEEL` message at the verified same-process HWND and screen
coordinate. This avoids reliance on the user's global focus/hover wheel routing
setting. A failed or timed-out dispatch is never retried through another path.

The helper uses `PrintWindow` for the selected HWND only, with no screen-capture
fallback. Unsupported or empty GPU captures report `CAPTURE_UNAVAILABLE`.
It refuses secure desktops, UAC/elevated applications, password targets, held
mouse buttons/modifiers, and unapproved windows. It never elevates, invokes a
shell, executes arbitrary commands or uses the clipboard. Windows input is
foreground input and does not promise background desktop control; use the
Chrome extension for background browser tasks.

Implementation references:

- [UI Automation threading](https://learn.microsoft.com/en-us/dotnet/framework/ui-automation/ui-automation-threading-issues)
- [PrintWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-printwindow)
- [SendInput and UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- [DPI awareness](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setprocessdpiawarenesscontext)
