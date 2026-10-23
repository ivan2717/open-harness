# Browser Use

A first-party Open Harness plugin with one Skill and a local stdio MCP server.
It uses Playwright Chromium in a new isolated browser context. It has no model
client, separate agent loop, user-profile access or arbitrary JavaScript tool.
This version supports macOS and Linux. The desktop marks it unavailable on
Windows because a Windows launcher is not yet provided.
Real Chromium and Electron acceptance tests were run on macOS. Linux display
selection has unit coverage but has not been verified on a Linux host.

## Setup

Use the desktop application's Browser Use setup entry to prepare Chromium and
install this plugin. Preparation downloads a matching browser explicitly;
installing or enabling the plugin does not download or launch anything. The
first `browser_open` starts a visible browser when a graphical session is
available. On Linux without `DISPLAY` or `WAYLAND_DISPLAY` it uses headless
Chromium; screenshots still show its viewport. The host's ordinary approval
policy still applies to invoking tools.

The desktop supplies these environment values to the server:

- `OPENHARNESS_NODE_BINARY`: Node or Electron executable used by `launch.sh`.
- `ELECTRON_RUN_AS_NODE`: `1` when using Electron as the Node runtime.
- `OPENHARNESS_BROWSER_RUNTIME`: absolute `playwright-core` module entry.
- `PLAYWRIGHT_BROWSERS_PATH`: the host-managed Chromium cache.
- `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, `XAUTHORITY`: host-provided
  graphical-session variables, or empty strings when absent.

For development, `node plugins/browser-use/server.mjs` also works when
`OPENHARNESS_BROWSER_RUNTIME` points to a local Playwright module. The runtime
may alternatively be installed normally next to the plugin. No package manager
is invoked by the server. `--headless` is available for automated tests.

## Tool contract

| Tool                 | Result                                                                         |
| -------------------- | ------------------------------------------------------------------------------ |
| `browser_open`       | Navigate to HTTP(S), optionally in a new tab; return a snapshot.               |
| `browser_snapshot`   | Visible text, accessibility tree, interactive element refs, tabs and revision. |
| `browser_action`     | Click, fill, select, press, scroll, back or forward; return a fresh snapshot.  |
| `browser_screenshot` | Viewport JPEG as an MCP image block with matching snapshot metadata.           |
| `browser_close`      | Close the entire isolated session and discard temporary cookies.               |

All calls are serialized within one MCP process. Actions must carry the latest
snapshot revision; page mutations, navigation, tab changes and viewport changes
invalidate observations. Semantic actions use live element handles belonging to
that snapshot. Coordinate clicks additionally require a screenshot taken at the
same revision. A page can still change between checking and dispatching, so an
accepted action is never reported as a verified task outcome.

Each attempted side effect is dispatched once. Playwright may wait for an
element to become actionable, but this server does not repeat navigation,
clicks or typing after failure. Error results mark uncertain outcomes. MCP
cancellation closes the session and invalidates queued work. EOF and process
termination close the browser; Open Harness also owns process-group cleanup.

Text and accessibility snapshots are bounded to 24,000 and 16,000 characters,
respectively, with at most 128 interactive refs. Screenshots are viewport-only
JPEGs capped at 2 MiB and never returned as text/base64 prose. The tool result's
image block must reach a vision-capable model through the host's multimodal
pipeline. Browser structure remains usable with text-only models.

The complete textual snapshot additionally stays below 28 KiB of serialized
MCP JSON (including UTF-8 and escaping), preserving the revision and refs inside
the host's 32 KiB result budget. Large pages set `truncated` and shorten text
before dropping excess refs.
Use `browser_snapshot` with its returned `next_text_offset` to continue reading
long page text. Element refs prioritize the current viewport, so scrolling can
reveal controls beyond the initial 128 entries. Restart text pagination after
navigation or page-content changes.

## Boundaries

This controls a separate browser, not the user's existing Chrome, desktop
applications, system dialogs or clipboard. HTTP(S) URL navigation is supported;
file URLs and credential-bearing URLs are rejected. Browser cookies are
ephemeral. Service workers and accepted downloads are disabled. File upload,
arbitrary evaluation, persisted profiles, network-domain allowlists and direct
iframe element refs are not exposed in this initial plugin. Cross-origin
iframe visuals may be observed in screenshots, but native dialogs need another
executor. The browser process is not a substitute for a host/OS security
sandbox, and web content is never permission to perform unrelated actions.

## Validation

Run from the repository root after preparing a matching Playwright Chromium:

```sh
node --test plugins/browser-use/tests/browser.test.mjs
```

Set `PLAYWRIGHT_BROWSERS_PATH` when using a non-default browser cache. These are
real Chromium tests against loopback HTTP fixtures, with no external website or
model API. They cover semantic reading, filling and one actual form submission,
navigation, popup tabs, stale refs, coordinate protection, screenshot image
blocks, serialized actions, uncertain failures, cancellation, fresh-session
cookies, browser shutdown and an MCP stdio roundtrip.
