import { spawnSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { stagePluginResources } from './plugin-resources.mjs'
import { stageManagedWorkspaceRuntime } from './workspace-runtime.mjs'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const source = resolve(desktop, '../../plugins')
await stageManagedWorkspaceRuntime({
  source: process.env.OPENHARNESS_PYTHON_RUNTIME_DIR,
  destination: resolve(desktop, 'resources/workspace-runtime'),
})
if (process.platform === 'win32') {
  // Reuse the static-CRT build configuration. Cargo's incremental check keeps
  // standalone `pnpm run build` and development launches current without building
  // the API or relying on a system Node.js installation in the shipped app.
  const build = spawnSync(
    process.execPath,
    [resolve(desktop, 'scripts/backend.mjs'), 'release', '--chrome-host-only'],
    { stdio: 'inherit' },
  )
  if (build.error) throw build.error
  if (build.status !== 0) process.exit(build.status ?? 1)
}
if (['darwin', 'win32'].includes(process.platform)) {
  const build = spawnSync(
    process.execPath,
    [
      resolve(
        source,
        process.platform === 'win32' ? 'computer-use/windows/build.mjs' : 'computer-use/build.mjs',
      ),
    ],
    { stdio: 'inherit' },
  )
  if (build.error) throw build.error
  if (build.status !== 0) process.exit(build.status ?? 1)
}
const destination = resolve(desktop, 'resources/plugins')
await stagePluginResources({
  source,
  destination,
  platform: process.platform,
  windowsLauncher: resolve(desktop, 'resources/bin/openharness-chrome-host.exe'),
})
