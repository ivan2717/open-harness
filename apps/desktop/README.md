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

The first version includes a **local, deterministic demonstration model**. It
uses the real `AgentRuntime`, tool execution, event persistence, and context
compaction. Its generated text and tool data are synthetic; token counts are
estimates. It does not make model API calls or validate any provider's model
quality. The small demonstration context window intentionally makes compaction
observable in a short run; it does not change the harness's production defaults.

Create a run from the composer, inspect its streaming output and tool steps,
and use the context panel to examine budgets and actual compaction checkpoints.
Run history is stored in the application's user-data directory and survives
restarts. An interrupted run is shown as interrupted instead of silently
re-executing tools on startup.

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
npm run package:dir
```

`build` compiles the desktop JavaScript and renderer. `package:dir` additionally
builds a release Rust binary, places it in the desktop resources, and creates an
unpacked application for the current OS and architecture. `npm run package`
creates the configured distributable. Public distribution still requires the
appropriate platform signing and notarization configuration.

`test:e2e` launches a real Electron window with isolated temporary data. It checks
creating and stopping runs, actual context compaction, restart recovery, and
renderer isolation, then saves screenshots under `test-results/`. It requires a
desktop display (or a suitable virtual display in Linux CI).

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
