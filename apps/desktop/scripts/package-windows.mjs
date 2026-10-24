import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

// The Rust API is built for the host. Cross-packaging Electron alone would put a
// macOS/Linux backend in a Windows installer, so require the matching host here.
if (process.platform !== 'win32' || process.arch !== 'x64') {
  throw new Error('Windows x64 packaging requires a Windows x64 host. Use the Checks workflow.')
}
if (!process.env.npm_execpath) throw new Error('Run this script with npm run package:win')

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(import.meta.url)
for (const args of [
  [resolve(desktop, 'scripts/backend.mjs'), 'release'],
  [process.env.npm_execpath, 'run', 'build'],
  [require.resolve('electron-builder/cli.js'), '--win', 'nsis', '--x64', '--publish', 'never'],
]) {
  // Use Node directly; spawning npm.cmd without a shell fails on Windows.
  const result = spawnSync(process.execPath, args, { cwd: desktop, stdio: 'inherit' })
  if (result.error) throw result.error
  if (result.status !== 0) process.exit(result.status ?? 1)
}
