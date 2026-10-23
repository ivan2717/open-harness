import { spawnSync } from 'node:child_process'
import { cp, mkdir, rm } from 'node:fs/promises'
import { dirname, resolve, basename } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const source = resolve(desktop, '../../plugins')
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
}
