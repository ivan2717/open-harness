import { spawnSync } from 'node:child_process'
import { chmod, copyFile, mkdir } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const workspace = resolve(desktop, '../..')
const release = process.argv[2] === 'release'
const build = spawnSync(
  'cargo',
  ['build', '--locked', '-p', 'api', ...(release ? ['--release'] : [])],
  {
    cwd: workspace,
    stdio: 'inherit',
  },
)
if (build.error) throw build.error
if (build.status !== 0) process.exit(build.status ?? 1)
if (release) {
  const filename = process.platform === 'win32' ? 'api.exe' : 'api'
  const targetDirectory = process.env.CARGO_TARGET_DIR
    ? resolve(workspace, process.env.CARGO_TARGET_DIR)
    : resolve(workspace, 'target')
  const resources = resolve(desktop, 'resources/bin')
  await mkdir(resources, { recursive: true })
  await copyFile(resolve(targetDirectory, 'release', filename), resolve(resources, filename))
  if (process.platform !== 'win32') await chmod(resolve(resources, filename), 0o755)
}
