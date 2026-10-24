import { spawnSync } from 'node:child_process'
import { copyFile, cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { dirname, resolve, basename } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const source = resolve(desktop, '../../plugins')
if (process.platform === 'win32') {
  // Reuse the static-CRT build configuration. Cargo's incremental check keeps
  // standalone `npm run build` and development launches current without building
  // the API or relying on a system Node.js installation in the shipped app.
  const build = spawnSync(
    process.execPath,
    [resolve(desktop, 'scripts/backend.mjs'), 'release', '--chrome-host-only'],
    { stdio: 'inherit' },
  )
  if (build.error) throw build.error
  if (build.status !== 0) process.exit(build.status ?? 1)
}
if (process.platform === 'darwin') {
  const build = spawnSync(process.execPath, [resolve(source, 'computer-use/build.mjs')], {
    stdio: 'inherit',
  })
  if (build.error) throw build.error
  if (build.status !== 0) process.exit(build.status ?? 1)
}
const destination = resolve(desktop, 'resources/plugins')
await mkdir(destination, { recursive: true })
for (const name of [
  'browser-use',
  'chrome-use',
  ...(process.platform === 'darwin' ? ['computer-use'] : []),
]) {
  const target = resolve(destination, name)
  await rm(target, { recursive: true, force: true })
  await cp(resolve(source, name), target, {
    recursive: true,
    filter: (path) => !['tests', 'dist', 'node_modules', '.DS_Store'].includes(basename(path)),
  })
  if (name === 'chrome-use' && process.platform === 'win32') {
    await copyFile(
      resolve(desktop, 'resources/bin/openharness-chrome-host.exe'),
      resolve(target, 'launch.exe'),
    )
    const manifestPath = resolve(target, 'mcp.json')
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
    manifest.mcpServers.chrome.command = './launch.exe'
    manifest.mcpServers.chrome.args = ['--mcp']
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`)
  }
}
