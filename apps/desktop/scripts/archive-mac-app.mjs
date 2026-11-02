import { execFile } from 'node:child_process'
import { lstat, mkdir, rename, rm } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { fileURLToPath } from 'node:url'

const execute = promisify(execFile)

/** Preserve rollback bytes without leaving an installable duplicate .app. */
export async function archiveMacApp(app, directory = join(dirname(app), 'archives')) {
  const info = await lstat(app)
  if (!app.endsWith('.app') || !info.isDirectory() || info.isSymbolicLink())
    throw new Error('Expected an ordinary .app directory')
  const id = await execute('/usr/bin/plutil', [
    '-extract',
    'CFBundleIdentifier',
    'raw',
    '-o',
    '-',
    join(app, 'Contents', 'Info.plist'),
  ])
  if (id.stdout.trim() !== 'dev.openharness.desktop')
    throw new Error('Only Open Harness desktop bundles may be archived')
  await mkdir(directory, { recursive: true })
  const archive = join(directory, `${basename(app, '.app')}-${Date.now()}.zip`)
  const temporary = `${archive}.partial`
  try {
    await execute('/usr/bin/ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', app, temporary])
    await execute('/usr/bin/unzip', ['-tq', temporary], { maxBuffer: 1024 * 1024 })
    await rename(temporary, archive)
    // This removes only our archived rollback copy, never user application data.
    await rm(app, { recursive: true })
    return archive
  } finally {
    await rm(temporary, { force: true })
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (!process.argv[2]) throw new Error('Usage: node scripts/archive-mac-app.mjs /path/to/old.app')
  console.log(await archiveMacApp(resolve(process.argv[2])))
}
