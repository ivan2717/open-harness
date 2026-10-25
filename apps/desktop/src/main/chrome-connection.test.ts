import {
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  rename,
  rm,
  writeFile,
  stat,
  chmod,
  symlink,
} from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ChromeConnection } from './chrome-connection'

vi.mock('node:fs/promises', async (importOriginal) => {
  const original = await importOriginal<typeof import('node:fs/promises')>()
  return { ...original, rename: vi.fn(original.rename) }
})

describe('Chrome Native Messaging registration', () => {
  let directory: string
  let connection: ChromeConnection
  let root: string
  let home: string
  const manifestPath = () =>
    join(
      home,
      'Library/Application Support/Google/Chrome/NativeMessagingHosts/dev.openharness.chrome.json',
    )
  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'oh-chrome-setup-'))
    root = join(directory, 'Open Harness.app', 'Contents', "resources with 'quotes'")
    home = join(directory, 'home')
    await mkdir(join(root, 'chrome-use', 'extension'), { recursive: true })
    const manifest = await readFile(
      resolve('../../plugins/chrome-use/extension/manifest.json'),
      'utf8',
    )
    await writeFile(join(root, 'chrome-use', 'extension', 'manifest.json'), manifest)
    await writeFile(join(root, 'chrome-use', 'extension', 'background.mjs'), '// background')
    await mkdir(join(root, 'chrome-use', 'extension', 'icons'))
    await writeFile(
      join(root, 'chrome-use', 'extension', 'icons', 'test.png'),
      Buffer.from([0, 1, 255]),
    )
    await writeFile(join(root, 'chrome-use', 'native-host.mjs'), '// not launched by setup')
    await writeFile(
      join(root, 'chrome-use', 'status.mjs'),
      'console.log(JSON.stringify({connected:true}))',
    )
    connection = new ChromeConnection({
      root,
      home,
      userData: join(directory, 'data'),
      executable: process.execPath,
      platform: 'darwin',
      socketRoot: join(directory, 'sockets'),
    })
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  it('does not register a host or launch Chrome during a status check', async () => {
    expect(await connection.status()).toMatchObject({
      configured: false,
      connected: false,
      extensionPath: join(directory, 'data', 'chrome-use', 'extension'),
    })
    await expect(readFile(manifestPath())).rejects.toMatchObject({ code: 'ENOENT' })
    await expect(stat(join(directory, 'data'))).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('does not create configuration or register an installation during startup refresh', async () => {
    await connection.refreshExisting()
    await expect(readFile(manifestPath())).rejects.toMatchObject({ code: 'ENOENT' })
    await expect(stat(join(directory, 'data'))).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it.each(['config', 'registration'])(
    'does not restore a removed %s during startup',
    async (removed) => {
      await connection.prepare()
      const path = removed === 'config' ? connection.configPath : manifestPath()
      await rm(path)
      await writeFile(join(root, 'chrome-use', 'extension', 'background.mjs'), '// newer')

      await connection.refreshExisting()

      await expect(stat(path)).rejects.toMatchObject({ code: 'ENOENT' })
      expect(await readFile(join(connection.extensionPath, 'background.mjs'), 'utf8')).toBe(
        '// background',
      )
    },
  )

  it('refreshes an existing installation after an app update without replacing credentials', async () => {
    await connection.prepare()
    const previousConfig = await readFile(connection.configPath, 'utf8')
    const exportedPath = connection.extensionPath
    const updatedRoot = join(directory, 'updated-package')
    await rename(root, updatedRoot)
    await writeFile(join(updatedRoot, 'chrome-use', 'extension', 'background.mjs'), '// newer')
    await writeFile(
      join(updatedRoot, 'chrome-use', 'status.mjs'),
      'console.log(JSON.stringify({connected:false}))',
    )
    const updatedConnection = new ChromeConnection({
      root: updatedRoot,
      home,
      userData: join(directory, 'data'),
      executable: process.execPath,
      platform: 'darwin',
      socketRoot: join(directory, 'sockets'),
    })

    await updatedConnection.refreshExisting()

    expect(updatedConnection.extensionPath).toBe(exportedPath)
    expect(await readFile(join(exportedPath, 'background.mjs'), 'utf8')).toBe('// newer')
    expect(await readFile(connection.configPath, 'utf8')).toBe(previousConfig)
    const manifest = JSON.parse(await readFile(manifestPath(), 'utf8'))
    expect(await readFile(manifest.path, 'utf8')).toContain(
      join(updatedRoot, 'chrome-use', 'native-host.mjs'),
    )
    expect(manifest.allowed_origins).toEqual([
      'chrome-extension://keibjmjjimfgkikahpjgbfebiglbjkee/',
    ])
    expect(await updatedConnection.status()).toMatchObject({ configured: true, connected: false })
  })

  it('keeps unchanged exported files in place on subsequent startups', async () => {
    await connection.prepare()
    const file = join(connection.extensionPath, 'background.mjs')
    const previous = await stat(file)

    await connection.refreshExisting()

    const current = await stat(file)
    expect(current.ino).toBe(previous.ino)
    expect(current.mtimeMs).toBe(previous.mtimeMs)
  })

  it('does not replace invalid existing credentials during automatic refresh', async () => {
    await connection.prepare()
    const invalid = JSON.stringify({ extensionId: 'another-extension', token: 'invalid' })
    await writeFile(connection.configPath, invalid)
    await writeFile(join(root, 'chrome-use', 'extension', 'background.mjs'), '// newer')

    await expect(connection.refreshExisting()).rejects.toThrow('连接配置无效')

    expect(await readFile(connection.configPath, 'utf8')).toBe(invalid)
    expect(await readFile(join(connection.extensionPath, 'background.mjs'), 'utf8')).toBe(
      '// background',
    )
  })

  it('does not suppress explicit setup while an unconfigured startup refresh is pending', async () => {
    await Promise.all([connection.refreshExisting(), connection.prepare()])
    expect(await connection.status()).toMatchObject({ configured: true, connected: true })
  })

  it('exports only the extension to an ordinary private directory outside the app bundle', async () => {
    await connection.prepare()
    expect(connection.extensionPath).toBe(join(directory, 'data', 'chrome-use', 'extension'))
    expect(connection.extensionPath).not.toContain('.app/')
    expect(await readFile(join(connection.extensionPath, 'manifest.json'))).toEqual(
      await readFile(join(root, 'chrome-use', 'extension', 'manifest.json')),
    )
    expect(await readFile(join(connection.extensionPath, 'background.mjs'), 'utf8')).toBe(
      '// background',
    )
    expect(await readFile(join(connection.extensionPath, 'icons', 'test.png'))).toEqual(
      Buffer.from([0, 1, 255]),
    )
    expect((await stat(connection.extensionPath)).mode & 0o777).toBe(0o700)
    expect((await stat(join(connection.extensionPath, 'manifest.json'))).mode & 0o777).toBe(0o600)
    expect(await readdir(connection.extensionPath)).toEqual([
      'background.mjs',
      'icons',
      'manifest.json',
    ])
  })

  it('updates the same exported path from the trusted package and removes obsolete files', async () => {
    await connection.prepare()
    const path = connection.extensionPath
    await writeFile(join(path, 'obsolete.mjs'), '// old')
    await writeFile(join(path, 'manifest.json'), '{"key":"untrusted export"}')
    await writeFile(join(root, 'chrome-use', 'extension', 'background.mjs'), '// upgraded')
    await connection.prepare()
    expect(connection.extensionPath).toBe(path)
    expect(await readFile(join(path, 'background.mjs'), 'utf8')).toBe('// upgraded')
    expect(await readFile(join(path, 'manifest.json'))).toEqual(
      await readFile(join(root, 'chrome-use', 'extension', 'manifest.json')),
    )
    await expect(stat(join(path, 'obsolete.mjs'))).rejects.toMatchObject({ code: 'ENOENT' })
    expect(
      (await readdir(join(path, '..'))).filter((name) => name.startsWith('.extension-')),
    ).toEqual([])
  })

  it('does not recreate or refresh an exported directory during status checks', async () => {
    await connection.prepare()
    await rm(connection.extensionPath, { recursive: true })
    expect(await connection.status()).toMatchObject({ extensionPath: connection.extensionPath })
    await expect(stat(connection.extensionPath)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it.each(['file', 'directory'])(
    'rejects a linked source %s and preserves the previous export',
    async (kind) => {
      await connection.prepare()
      const source = join(root, 'chrome-use', 'extension')
      const outside = join(directory, 'untrusted')
      if (kind === 'directory') await mkdir(outside)
      else await writeFile(outside, 'must not be exported')
      await symlink(outside, join(source, 'linked'))
      await writeFile(join(source, 'background.mjs'), '// not published')

      await expect(connection.prepare()).rejects.toThrow('符号链接')
      expect(await readFile(join(connection.extensionPath, 'background.mjs'), 'utf8')).toBe(
        '// background',
      )
      await expect(stat(join(connection.extensionPath, 'linked'))).rejects.toMatchObject({
        code: 'ENOENT',
      })
      expect(
        (await readdir(join(connection.extensionPath, '..'))).filter((name) =>
          name.startsWith('.extension-'),
        ),
      ).toEqual([])
    },
  )

  it('rejects a symlink replacing the packaged extension directory', async () => {
    const source = join(root, 'chrome-use', 'extension')
    const outside = join(directory, 'untrusted')
    await rename(source, outside)
    await symlink(outside, source)
    await expect(connection.prepare()).rejects.toThrow('符号链接')
    await expect(stat(connection.extensionPath)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects a symlink replacing the export without touching its target', async () => {
    await connection.prepare()
    const outside = join(directory, 'unrelated-directory')
    await mkdir(outside)
    await writeFile(join(outside, 'keep.txt'), 'original')
    await rm(connection.extensionPath, { recursive: true })
    await symlink(outside, connection.extensionPath)
    await expect(connection.prepare()).rejects.toThrow('符号链接')
    expect(await readdir(outside)).toEqual(['keep.txt'])
    expect(await readFile(join(outside, 'keep.txt'), 'utf8')).toBe('original')
  })

  it('rejects a linked export parent instead of writing outside userData', async () => {
    const outside = join(directory, 'unrelated-directory')
    await mkdir(outside)
    await mkdir(join(directory, 'data'))
    await symlink(outside, join(directory, 'data', 'chrome-use'))
    await expect(connection.prepare()).rejects.toThrow('目录不安全')
    expect(await readdir(outside)).toEqual([])
  })

  it('rejects ambiguous path separators in package entries', async () => {
    await writeFile(join(root, 'chrome-use', 'extension', '..\\escape.mjs'), '// invalid')
    await expect(connection.prepare()).rejects.toThrow('文件路径')
    await expect(stat(connection.extensionPath)).rejects.toMatchObject({ code: 'ENOENT' })
    await expect(stat(join(directory, 'data', 'chrome-use', 'escape.mjs'))).rejects.toMatchObject({
      code: 'ENOENT',
    })
  })

  it('rolls back the previous export if publishing the staged directory fails', async () => {
    await connection.prepare()
    await writeFile(join(root, 'chrome-use', 'extension', 'background.mjs'), '// replacement')
    const original = await vi.importActual<typeof import('node:fs/promises')>('node:fs/promises')
    vi.mocked(rename).mockImplementation(async (from, to) => {
      if (to === connection.extensionPath && !String(from).endsWith('.previous')) {
        throw new Error('simulated publish failure')
      }
      await original.rename(from, to)
    })
    try {
      await expect(connection.prepare()).rejects.toThrow('simulated publish failure')
      expect(await readFile(join(connection.extensionPath, 'background.mjs'), 'utf8')).toBe(
        '// background',
      )
      expect(
        (await readdir(join(connection.extensionPath, '..'))).filter((name) =>
          name.startsWith('.extension-'),
        ),
      ).toEqual([])
    } finally {
      vi.mocked(rename).mockImplementation(original.rename)
    }
  })

  it('registers only the bundled extension and keeps credentials private and stable across setup', async () => {
    await connection.prepare()
    const manifest = JSON.parse(await readFile(manifestPath(), 'utf8'))
    expect(manifest.allowed_origins).toEqual([
      'chrome-extension://keibjmjjimfgkikahpjgbfebiglbjkee/',
    ])
    expect(manifest.type).toBe('stdio')
    const config = JSON.parse(await readFile(connection.configPath, 'utf8'))
    expect(config.token).toMatch(/^[a-f0-9]{64}$/)
    expect((await stat(connection.configPath)).mode & 0o777).toBe(0o600)
    expect((await stat(join(connection.configPath, '..'))).mode & 0o777).toBe(0o700)
    const launcher = await readFile(manifest.path, 'utf8')
    expect(launcher).toContain("'\\''quotes'\\''")
    expect(launcher).toContain('"$@"')
    await connection.prepare()
    expect(JSON.parse(await readFile(connection.configPath, 'utf8')).token).toBe(config.token)
    expect(await connection.status()).toMatchObject({ configured: true, connected: true })
  })

  it('rejects a substituted extension before registering its host', async () => {
    await writeFile(
      join(root, 'chrome-use', 'extension', 'manifest.json'),
      JSON.stringify({ key: Buffer.from('other extension').toString('base64') }),
    )
    await expect(connection.prepare()).rejects.toThrow('身份')
    await expect(readFile(manifestPath())).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects a public or symlinked config without leaking or replacing it', async () => {
    await connection.prepare()
    await chmod(connection.configPath, 0o644)
    await expect(connection.prepare()).rejects.toThrow('文件权限')
    await rm(connection.configPath)
    const target = join(directory, 'do-not-overwrite')
    await writeFile(target, 'original')
    await symlink(target, connection.configPath)
    await expect(connection.prepare()).rejects.toThrow('文件权限')
    expect(await readFile(target, 'utf8')).toBe('original')
  })

  it('distinguishes disconnected and malformed status results', async () => {
    await connection.prepare()
    const probe = join(root, 'chrome-use', 'status.mjs')
    await writeFile(probe, 'console.log(JSON.stringify({connected:false}))')
    expect(await connection.status()).toMatchObject({ configured: true, connected: false })
    await writeFile(probe, 'console.log(JSON.stringify({connected:"yes"}))')
    expect(await connection.status()).toMatchObject({ connected: false, error: expect.any(String) })
  })

  const storeUrl =
    'https://chromewebstore.google.com/detail/open-harness-chrome/keibjmjjimfgkikahpjgbfebiglbjkee'
  const actions = () => ({
    enablePlugin: vi.fn(async () => {}),
    openDirectory: vi.fn(async (_path: string) => {}),
    openChrome: vi.fn(async (_url: string) => {}),
  })
  const distribution = (url: unknown) =>
    writeFile(
      join(root, 'chrome-use', 'distribution.json'),
      JSON.stringify({ chromeWebStoreUrl: url }),
    )

  it('does not pretend an unpublished extension can install, or change anything', async () => {
    const effects = actions()
    await distribution(null)
    expect(await connection.status()).toMatchObject({ storeUrl: null })
    await expect(connection.connect('store', effects)).rejects.toThrow('尚未上架')
    expect(effects.enablePlugin).not.toHaveBeenCalled()
    expect(effects.openChrome).not.toHaveBeenCalled()
    await expect(readFile(manifestPath())).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('registers the native host and enables the plugin before opening the verified listing', async () => {
    await distribution(storeUrl)
    const effects = actions()
    effects.openChrome.mockImplementation(async (url) => {
      expect(url).toBe(storeUrl)
      expect(effects.enablePlugin).toHaveBeenCalledOnce()
      expect(JSON.parse(await readFile(manifestPath(), 'utf8')).allowed_origins).toHaveLength(1)
    })
    await connection.connect('store', effects)
    expect(effects.openChrome).toHaveBeenCalledOnce()
    expect(effects.openDirectory).not.toHaveBeenCalled()
    expect(await connection.status()).toMatchObject({ storeUrl })
  })

  it.each([
    'https://example.com/detail/keibjmjjimfgkikahpjgbfebiglbjkee',
    'https://chromewebstore.google.com/detail/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    `${storeUrl}?redirect=https://example.com`,
    storeUrl.replace('https://', 'http://'),
    storeUrl.replace('https://', 'https://user:password@'),
    storeUrl.replace('.com/', '.com.evil.test/'),
    42,
  ])('rejects an invalid listing %s before any installation effect', async (url) => {
    await distribution(url)
    const effects = actions()
    expect(await connection.status()).toMatchObject({
      storeUrl: null,
      installationError: expect.any(String),
    })
    await expect(connection.connect('store', effects)).rejects.toThrow()
    expect(effects.enablePlugin).not.toHaveBeenCalled()
    expect(effects.openChrome).not.toHaveBeenCalled()
    await expect(readFile(manifestPath())).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('keeps unpacked loading explicit and does not confuse repair with installation', async () => {
    const effects = actions()
    await connection.connect('development', effects)
    expect(effects.openDirectory).toHaveBeenCalledWith(connection.extensionPath)
    expect(effects.openChrome).toHaveBeenCalledWith(
      'chrome://extensions/?id=keibjmjjimfgkikahpjgbfebiglbjkee',
    )
    effects.openDirectory.mockClear()
    await connection.connect('repair', effects)
    expect(effects.openDirectory).not.toHaveBeenCalled()
    await expect(connection.connect('https://example.com', effects)).rejects.toThrow('安装方式')
  })

  it('does not launch Chrome after plugin setup fails or report a failed launch as success', async () => {
    await distribution(storeUrl)
    const effects = actions()
    effects.enablePlugin.mockRejectedValueOnce(new Error('plugin setup failed'))
    await expect(connection.connect('store', effects)).rejects.toThrow('plugin setup failed')
    expect(effects.openChrome).not.toHaveBeenCalled()
    effects.openChrome.mockRejectedValueOnce(new Error('Chrome is missing'))
    await expect(connection.connect('store', effects)).rejects.toThrow('Chrome is missing')
  })
})
