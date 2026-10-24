import { execFile } from 'node:child_process'
import { constants } from 'node:fs'
import {
  access,
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  open,
  readFile,
  readdir,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createHash, randomBytes } from 'node:crypto'
import { join } from 'node:path'
import { promisify } from 'node:util'
import type { BundledPlugin } from '../shared/extensions'
import {
  registerWindowsChromeHost,
  secureWindowsChromeDirectory,
  windowsChromeHostRegistered,
} from './chrome-windows'

const execute = promisify(execFile)
const EXTENSION_ID = 'keibjmjjimfgkikahpjgbfebiglbjkee'
const HOST_NAME = 'dev.openharness.chrome'

/** User-initiated Native Messaging registration. No Chrome profile is modified. */
export class ChromeConnection {
  readonly configPath: string
  readonly extensionPath: string
  private readonly directory: string
  private readonly bundledExtensionPath: string
  private preparing?: Promise<void>

  constructor(
    private options: {
      root: string
      executable: string
      userData: string
      home: string
      platform: NodeJS.Platform
      socketRoot?: string
    },
  ) {
    this.directory = join(options.userData, 'chrome-use')
    this.configPath = join(this.directory, 'config.json')
    // Chrome's native folder picker cannot navigate into a packaged .app.
    // Keep unpacked installations at a stable, ordinary user-owned directory.
    this.extensionPath = join(this.directory, 'extension')
    this.bundledExtensionPath = join(options.root, 'chrome-use', 'extension')
  }

  async status(): Promise<NonNullable<BundledPlugin['chrome']>> {
    const base: NonNullable<BundledPlugin['chrome']> = {
      configured: false,
      connected: false,
      extensionPath: this.extensionPath,
      storeUrl: null,
    }
    try {
      base.storeUrl = await this.storeUrl()
    } catch {
      base.installationError = '当前安装包的 Chrome 商店信息无效，请更新 Open Harness。'
    }
    try {
      await access(this.configPath)
      await access(this.manifestPath())
      if (
        this.options.platform === 'win32' &&
        !(await windowsChromeHostRegistered(this.manifestPath()))
      )
        return base
    } catch {
      return base
    }
    try {
      const { stdout } = await execute(
        this.options.executable,
        [join(this.options.root, 'chrome-use', 'status.mjs')],
        {
          env: {
            ...process.env,
            ELECTRON_RUN_AS_NODE: '1',
            OPENHARNESS_CHROME_CONFIG: this.configPath,
          },
          timeout: 5_000,
          maxBuffer: 16_384,
          windowsHide: true,
        },
      )
      const result: unknown = JSON.parse(stdout)
      if (
        !result ||
        typeof result !== 'object' ||
        !('connected' in result) ||
        typeof result.connected !== 'boolean'
      )
        throw new Error('Invalid status')
      return {
        ...base,
        configured: true,
        connected: result.connected,
        allowNewTabs: 'allow_new_tabs' in result && result.allow_new_tabs === true,
      }
    } catch {
      return { ...base, configured: true, error: '无法检查 Chrome 连接，请打开扩展后重试。' }
    }
  }

  /** Validate distribution before any setup side effect. Never invent a listing. */
  private async storeUrl(): Promise<string | null> {
    let distribution: { chromeWebStoreUrl?: unknown }
    try {
      distribution = JSON.parse(
        await readFile(join(this.options.root, 'chrome-use', 'distribution.json'), 'utf8'),
      )
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null
      throw error
    }
    if (
      !distribution ||
      typeof distribution !== 'object' ||
      Array.isArray(distribution) ||
      Object.keys(distribution).length !== 1
    )
      throw new Error('Invalid distribution')
    if (distribution.chromeWebStoreUrl === null) return null
    if (typeof distribution.chromeWebStoreUrl !== 'string') throw new Error('Invalid store URL')
    const url = new URL(distribution.chromeWebStoreUrl)
    if (
      url.origin !== 'https://chromewebstore.google.com' ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      !new RegExp(`^/detail/(?:[a-z0-9][a-z0-9-]*/)?${EXTENSION_ID}/?$`).test(url.pathname)
    )
      throw new Error('Invalid store URL or extension identity')
    return url.href
  }

  async connect(
    mode: unknown,
    actions: {
      enablePlugin: () => Promise<void>
      openDirectory: (path: string) => Promise<void>
      openChrome: (url: string) => Promise<void>
    },
  ): Promise<void> {
    if (mode !== 'store' && mode !== 'development' && mode !== 'repair')
      throw new Error('无效的 Chrome 安装方式。')
    const url = mode === 'store' ? await this.storeUrl() : null
    if (mode === 'store' && !url) throw new Error('Chrome 扩展尚未上架，暂不能从商店安装。')
    await this.prepare()
    await actions.enablePlugin()
    if (mode === 'development') await actions.openDirectory(this.extensionPath)
    await actions.openChrome(url ?? `chrome://extensions/?id=${EXTENSION_ID}`)
  }

  prepare(): Promise<void> {
    if (this.preparing) return this.preparing
    this.preparing = this.register().finally(() => {
      this.preparing = undefined
    })
    return this.preparing
  }

  private manifestPath(): string {
    if (this.options.platform === 'win32') return join(this.directory, `${HOST_NAME}.json`)
    const directory =
      this.options.platform === 'darwin'
        ? join(
            this.options.home,
            'Library',
            'Application Support',
            'Google',
            'Chrome',
            'NativeMessagingHosts',
          )
        : join(this.options.home, '.config', 'google-chrome', 'NativeMessagingHosts')
    return join(directory, `${HOST_NAME}.json`)
  }

  private async register(): Promise<void> {
    if (!['darwin', 'linux', 'win32'].includes(this.options.platform))
      throw new Error('目前支持 Windows、macOS 和 Linux 上的 Chrome。')
    await ordinaryDirectory(this.options.root)
    await ordinaryDirectory(join(this.options.root, 'chrome-use'))
    await ordinaryDirectory(this.bundledExtensionPath)
    const nativeHost = join(this.options.root, 'chrome-use', 'native-host.mjs')
    if (!(await lstat(nativeHost)).isFile()) throw new Error('Chrome 连接程序无效。')
    await validateExtensionIdentity(this.bundledExtensionPath)
    const windows = this.options.platform === 'win32'
    await privateDirectory(this.directory, windows)
    if (windows) await secureWindowsChromeDirectory(this.directory)
    // A short private path avoids macOS sockaddr_un's path limit, even when
    // userData lives in a deeply nested folder. Each desktop profile is isolated.
    const hash = createHash('sha256').update(this.options.userData).digest('hex').slice(0, 16)
    const socketDirectory = join(
      this.options.socketRoot ?? '/tmp',
      `oh-chrome-${process.getuid?.() ?? 'user'}-${hash}`,
    )
    if (!windows) await privateDirectory(socketDirectory)
    let token = randomBytes(32).toString('hex')
    try {
      const existing = await lstat(this.configPath)
      if (existing.isSymbolicLink()) throw new Error('Unsafe config')
      const handle = await open(
        this.configPath,
        constants.O_RDONLY | (windows ? 0 : constants.O_NOFOLLOW),
      )
      try {
        const stat = await handle.stat()
        if (!stat.isFile() || (!windows && (stat.uid !== process.getuid?.() || stat.mode & 0o077)))
          throw new Error('Unsafe config')
        const previous = JSON.parse(await handle.readFile('utf8'))
        if (previous.extensionId === EXTENSION_ID && /^[a-f0-9]{64}$/.test(previous.token))
          token = previous.token
      } finally {
        await handle.close()
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT')
        throw new Error('Chrome 连接配置无效，请检查本地配置文件权限。')
    }
    await exportExtension(this.bundledExtensionPath, this.extensionPath)
    await atomicWrite(
      this.configPath,
      JSON.stringify({
        version: 1,
        socketPath: windows
          ? `\\\\.\\pipe\\openharness-chrome-${createHash('sha256').update(this.options.userData).update(token).digest('hex').slice(0, 32)}`
          : join(socketDirectory, 'bridge.sock'),
        token,
        extensionId: EXTENSION_ID,
      }) + '\n',
      0o600,
    )
    const launcher = windows
      ? await this.windowsLauncher(nativeHost)
      : join(this.directory, 'native-host')
    if (!windows)
      await atomicWrite(
        launcher,
        [
          '#!/bin/sh',
          `export ELECTRON_RUN_AS_NODE=1`,
          `export OPENHARNESS_CHROME_CONFIG=${shellQuote(this.configPath)}`,
          `exec ${shellQuote(this.options.executable)} ${shellQuote(nativeHost)} "$@"`,
          '',
        ].join('\n'),
        0o700,
      )
    const manifest = this.manifestPath()
    await mkdir(join(manifest, '..'), { recursive: true })
    await atomicWrite(
      manifest,
      JSON.stringify(
        {
          name: HOST_NAME,
          description: 'Open Harness Chrome connection',
          path: launcher,
          type: 'stdio',
          allowed_origins: [`chrome-extension://${EXTENSION_ID}/`],
        },
        null,
        2,
      ) + '\n',
      0o600,
    )
    if (windows) await registerWindowsChromeHost(manifest)
  }

  private async windowsLauncher(script: string): Promise<string> {
    const bytes = await regularFile(
      join(this.options.root, '..', 'bin', 'openharness-chrome-host.exe'),
    )
    // A versioned name permits upgrades while Chrome still owns an older host.
    const hash = createHash('sha256').update(bytes).digest('hex').slice(0, 16)
    const launcher = join(this.directory, `native-host-${hash}.exe`)
    try {
      const existing = await regularFile(launcher)
      if (!existing.equals(bytes)) throw new Error('Chrome 连接程序校验失败。')
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
      await writeFile(launcher, bytes, { flag: 'wx', mode: 0o700 })
    }
    await atomicWrite(
      join(this.directory, 'native-launcher.json'),
      JSON.stringify({
        version: 1,
        executable: this.options.executable,
        script,
        config: this.configPath,
      }) + '\n',
      0o600,
    )
    return launcher
  }
}

async function ordinaryDirectory(path: string): Promise<void> {
  const stat = await lstat(path)
  if (!stat.isDirectory() || stat.isSymbolicLink())
    throw new Error('Chrome 扩展目录无效，不能使用符号链接。')
}

async function validateExtensionIdentity(path: string): Promise<void> {
  const extension = JSON.parse((await regularFile(join(path, 'manifest.json'))).toString('utf8'))
  if (typeof extension?.key !== 'string') throw new Error('Chrome 扩展身份与当前安装包不一致。')
  const id = createHash('sha256')
    .update(Buffer.from(extension.key, 'base64'))
    .digest('hex')
    .slice(0, 32)
    .replace(/[0-9a-f]/g, (hex) => String.fromCharCode(97 + parseInt(hex, 16)))
  if (id !== EXTENSION_ID) throw new Error('Chrome 扩展身份与当前安装包不一致。')
}

/** Do not follow links or read devices from either the package or an export. */
async function regularFile(path: string): Promise<Buffer> {
  const stat = await lstat(path)
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error('Chrome 扩展包含无效文件。')
  const flags = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW | constants.O_NONBLOCK
  const handle = await open(path, constants.O_RDONLY | flags)
  try {
    if (!(await handle.stat()).isFile()) throw new Error('Chrome 扩展包含无效文件。')
    return await handle.readFile()
  } finally {
    await handle.close()
  }
}

async function copyExtension(source: string, target: string): Promise<void> {
  await ordinaryDirectory(source)
  for (const entry of await readdir(source, { withFileTypes: true })) {
    if (entry.name === '.' || entry.name === '..' || /[/\\]/.test(entry.name))
      throw new Error('Chrome 扩展包含无效文件路径。')
    const from = join(source, entry.name)
    const to = join(target, entry.name)
    if (entry.isDirectory()) {
      await mkdir(to, { mode: 0o700 })
      await copyExtension(from, to)
    } else if (entry.isFile()) {
      await writeFile(to, await regularFile(from), { flag: 'wx', mode: 0o600 })
    } else {
      throw new Error('Chrome 扩展包含符号链接或无效文件。')
    }
  }
}

async function exportExtension(source: string, target: string): Promise<void> {
  // Build the complete replacement first. Failed copies never disturb the
  // directory Chrome may already have loaded, and removed package files vanish.
  const stage = await mkdtemp(join(target, '..', '.extension-'))
  const backup = `${stage}.previous`
  let previous = false
  try {
    await copyExtension(source, stage)
    await validateExtensionIdentity(stage)
    try {
      await ordinaryDirectory(target)
      previous = true
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
    }
    if (previous) await rename(target, backup)
    try {
      await rename(stage, target)
    } catch (error) {
      if (previous) await rename(backup, target)
      throw error
    }
    if (previous) await rm(backup, { recursive: true, force: true })
  } finally {
    await rm(stage, { recursive: true, force: true })
  }
}

async function privateDirectory(path: string, windows = false): Promise<void> {
  await mkdir(path, { recursive: true, mode: 0o700 })
  const stat = await lstat(path)
  if (!stat.isDirectory() || stat.isSymbolicLink() || (!windows && stat.uid !== process.getuid?.()))
    throw new Error('Chrome 连接目录不安全。')
  if (!windows) await chmod(path, 0o700)
}

async function atomicWrite(path: string, text: string, mode: number): Promise<void> {
  const temporary = `${path}.${randomBytes(8).toString('hex')}.tmp`
  try {
    await writeFile(temporary, text, { mode, flag: 'wx' })
    await rename(temporary, path)
  } finally {
    await rm(temporary, { force: true })
  }
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`
}
