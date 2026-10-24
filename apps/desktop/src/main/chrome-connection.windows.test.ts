import { mkdtemp, mkdir, readFile, rm, writeFile, cp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { beforeEach, afterEach, describe, it, expect, vi } from 'vitest'
import { ChromeConnection } from './chrome-connection'
import {
  registerWindowsChromeHost,
  secureWindowsChromeDirectory,
  windowsChromeHostRegistered,
  windowsNativeHostPath,
} from './chrome-windows'

vi.mock('./chrome-windows', () => ({
  registerWindowsChromeHost: vi.fn(async () => {}),
  secureWindowsChromeDirectory: vi.fn(async () => {}),
  windowsChromeHostRegistered: vi.fn(async () => true),
  windowsNativeHostPath: vi.fn(async (path: string) => path),
}))

describe('Windows Chrome connection preparation', () => {
  let directory: string
  let root: string
  let connection: ChromeConnection
  beforeEach(async () => {
    vi.clearAllMocks()
    vi.mocked(windowsChromeHostRegistered).mockResolvedValue(true)
    vi.mocked(windowsNativeHostPath).mockImplementation(async (path) => path)
    directory = await mkdtemp(join(tmpdir(), 'oh-chrome-windows-'))
    root = join(directory, 'resources', 'plugins')
    await mkdir(join(root, 'chrome-use'), { recursive: true })
    await mkdir(join(root, '..', 'bin'))
    await cp(resolve('../../plugins/chrome-use/extension'), join(root, 'chrome-use', 'extension'), {
      recursive: true,
    })
    await writeFile(
      join(root, '..', 'bin', 'openharness-chrome-host.exe'),
      'fixture binary is never executed',
    )
    await writeFile(join(root, 'chrome-use', 'native-host.mjs'), '// fixture')
    await writeFile(
      join(root, 'chrome-use', 'status.mjs'),
      'console.log(JSON.stringify({connected:true}))',
    )
    connection = new ChromeConnection({
      root,
      home: directory,
      userData: join(directory, 'data'),
      executable: process.execPath,
      platform: 'win32',
    })
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  it('prepares an authenticated named pipe and an executable host without POSIX scripts', async () => {
    await connection.prepare()
    const config = JSON.parse(await readFile(connection.configPath, 'utf8'))
    expect(config.socketPath).toMatch(/^\\\\\.\\pipe\\openharness-chrome-[a-f0-9]{32}$/)
    const manifestPath = join(directory, 'data', 'chrome-use', 'dev.openharness.chrome.json')
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
    expect(manifest.path).toMatch(/native-host-[a-f0-9]{16}\.exe$/)
    expect(windowsNativeHostPath).toHaveBeenCalledWith(manifest.path)
    expect(manifest.allowed_origins).toEqual([
      'chrome-extension://keibjmjjimfgkikahpjgbfebiglbjkee/',
    ])
    expect(await readFile(manifest.path, 'utf8')).toBe('fixture binary is never executed')
    const launcher = JSON.parse(
      await readFile(join(directory, 'data', 'chrome-use', 'native-launcher.json'), 'utf8'),
    )
    expect(launcher).toEqual({
      version: 1,
      executable: process.execPath,
      script: join(root, 'chrome-use', 'native-host.mjs'),
      config: connection.configPath,
    })
    expect(registerWindowsChromeHost).toHaveBeenCalledWith(manifestPath)
    expect(secureWindowsChromeDirectory).toHaveBeenCalledWith(join(directory, 'data', 'chrome-use'))
    await connection.prepare()
    expect(JSON.parse(await readFile(connection.configPath, 'utf8'))).toEqual(config)
    expect(await connection.status()).toMatchObject({ configured: true, connected: true })
  })

  it('reports missing registration without modifying it during status checks', async () => {
    await connection.prepare()
    vi.clearAllMocks()
    vi.mocked(windowsChromeHostRegistered).mockResolvedValue(false)
    expect(await connection.status()).toMatchObject({ configured: false, connected: false })
    expect(registerWindowsChromeHost).not.toHaveBeenCalled()
    expect(secureWindowsChromeDirectory).not.toHaveBeenCalled()
  })

  it('registers the verified alias while keeping the original private configuration path', async () => {
    const alias = String.raw`C:\Users\USER~1\CHROME~1\NATIVE~1.EXE`
    vi.mocked(windowsNativeHostPath).mockResolvedValue(alias)
    await connection.prepare()
    const manifest = JSON.parse(
      await readFile(join(directory, 'data', 'chrome-use', 'dev.openharness.chrome.json'), 'utf8'),
    )
    expect(manifest.path).toBe(alias)
    const launcher = JSON.parse(
      await readFile(join(directory, 'data', 'chrome-use', 'native-launcher.json'), 'utf8'),
    )
    expect(launcher.config).toBe(connection.configPath)
    expect(windowsNativeHostPath).toHaveBeenCalledOnce()
  })

  it('uses a new executable path during upgrades without replacing the previous host', async () => {
    await connection.prepare()
    const manifestPath = join(directory, 'data', 'chrome-use', 'dev.openharness.chrome.json')
    const old = JSON.parse(await readFile(manifestPath, 'utf8')).path
    await writeFile(join(root, '..', 'bin', 'openharness-chrome-host.exe'), 'new binary')
    await connection.prepare()
    const current = JSON.parse(await readFile(manifestPath, 'utf8')).path
    expect(current).not.toBe(old)
    expect(await readFile(old, 'utf8')).toBe('fixture binary is never executed')
    expect(await readFile(current, 'utf8')).toBe('new binary')
  })
})
