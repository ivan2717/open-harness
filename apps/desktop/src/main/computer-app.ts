import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { constants } from 'node:fs'
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  open,
  readdir,
  realpath,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises'
import { isAbsolute, join, resolve, sep } from 'node:path'
import { promisify } from 'node:util'

const execute = promisify(execFile)
export const COMPUTER_APP_NAME = 'Open Harness Computer Use.app'
const BUNDLE_ID = 'dev.openharness.computeruse'

/** Keep TCC's permission subject independent of the enclosing desktop .app.
 * Deploy existing signed bytes only; never sign, launch, or request permission.
 */
export async function installComputerApp(source: string, userData: string): Promise<string> {
  if (!isAbsolute(source) || !isAbsolute(userData)) throw new Error('电脑组件路径无效。')
  await ordinaryDirectory(source)
  const sourceDigest = await bundleDigest(source)
  await verifyBundle(source)
  await mkdir(userData, { recursive: true })
  await ordinaryDirectory(userData, true)
  const canonicalData = await realpath(userData)
  if (canonicalData.split(sep).some((part) => /\.app$/i.test(part)))
    throw new Error('电脑组件必须安装在应用包以外的普通目录。')
  const directory = join(canonicalData, 'computer-use')
  await mkdir(directory, { recursive: true, mode: 0o700 })
  await ordinaryDirectory(directory, true)
  await chmod(directory, 0o700)
  const target = join(directory, COMPUTER_APP_NAME)
  if (resolve(source) === target) throw new Error('电脑组件源目录与安装目录不能相同。')
  let previous = false
  try {
    await ordinaryDirectory(target, true)
    previous = true
    if ((await bundleDigest(target)) === sourceDigest) {
      await verifyBundle(target)
      return target
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
  }

  const stage = await mkdtemp(join(directory, '.computer-use-'))
  const candidate = join(stage, COMPUTER_APP_NAME)
  const backup = join(stage, 'previous.app')
  let published = false
  try {
    await copyBundle(source, candidate)
    if ((await bundleDigest(candidate)) !== sourceDigest)
      throw new Error('电脑组件在复制过程中发生变化，请重试。')
    await verifyBundle(candidate)
    if (previous) await rename(target, backup)
    try {
      await rename(candidate, target)
      published = true
    } catch (error) {
      if (previous) await rename(backup, target)
      throw error
    }
    return target
  } finally {
    // A failed rollback must preserve the last signed bundle for recovery.
    const strandedBackup = await lstat(backup).then(
      () => true,
      () => false,
    )
    if (!strandedBackup || published) await rm(stage, { recursive: true, force: true })
  }
}

async function verifyBundle(path: string): Promise<void> {
  const executable = join(path, 'Contents', 'MacOS', 'OpenHarnessComputerUse')
  const info = await lstat(executable)
  if (!info.isFile() || info.isSymbolicLink() || !(info.mode & 0o111))
    throw new Error('电脑组件的可执行文件无效。')
  const options = { timeout: 10_000, maxBuffer: 16_384 }
  await execute('/usr/bin/codesign', ['--verify', '--strict', path], options)
  const signature = await execute('/usr/bin/codesign', ['--display', '--verbose=2', path], options)
  if (
    /^Identifier=(.+)$/m.exec(`${signature.stdout}\n${signature.stderr}`)?.[1]?.trim() !== BUNDLE_ID
  )
    throw new Error('电脑组件的签名身份不匹配。')
}

async function ordinaryDirectory(path: string, owned = false): Promise<void> {
  const info = await lstat(path)
  if (!info.isDirectory() || info.isSymbolicLink() || (owned && info.uid !== process.getuid?.()))
    throw new Error('电脑组件目录不安全，不能使用符号链接。')
}

async function regularFile(path: string): Promise<{ bytes: Buffer; mode: number }> {
  const info = await lstat(path)
  if (!info.isFile() || info.isSymbolicLink()) throw new Error('电脑组件包含无效文件或符号链接。')
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const current = await handle.stat()
    if (!current.isFile()) throw new Error('电脑组件包含无效文件。')
    return { bytes: await handle.readFile(), mode: current.mode & 0o777 }
  } finally {
    await handle.close()
  }
}

async function entries(directory: string) {
  await ordinaryDirectory(directory)
  const result = await readdir(directory, { withFileTypes: true })
  for (const entry of result)
    if (/[/\\]/.test(entry.name) || (!entry.isDirectory() && !entry.isFile()))
      throw new Error('电脑组件包含无效路径或符号链接。')
  return result.sort((a, b) => a.name.localeCompare(b.name))
}

async function bundleDigest(directory: string): Promise<string> {
  const hash = createHash('sha256')
  for (const entry of await entries(directory)) {
    hash.update(JSON.stringify([entry.name, entry.isDirectory()]))
    const path = join(directory, entry.name)
    if (entry.isDirectory()) hash.update(await bundleDigest(path))
    else {
      const { bytes, mode } = await regularFile(path)
      hash.update(JSON.stringify(mode))
      hash.update(createHash('sha256').update(bytes).digest('hex'))
    }
  }
  return hash.digest('hex')
}

async function copyBundle(source: string, target: string): Promise<void> {
  await mkdir(target, { mode: 0o700 })
  for (const entry of await entries(source)) {
    const from = join(source, entry.name)
    const to = join(target, entry.name)
    if (entry.isDirectory()) await copyBundle(from, to)
    else {
      const { bytes, mode } = await regularFile(from)
      await writeFile(to, bytes, { mode, flag: 'wx' })
      // An unusual umask must not remove executable bits from the signed helper.
      await chmod(to, mode)
    }
  }
}
