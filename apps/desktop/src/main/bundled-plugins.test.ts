import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Backend } from './backend'
import { BundledPlugins } from './bundled-plugins'

describe('host-owned browser and computer plugin setup', () => {
  let directory: string
  let root: string
  let runtime: string
  let browser: string
  let setupMarker: string
  let plugins: BundledPlugins

  async function computerHelper(source: string, statusTimeoutMs?: number) {
    const path = join(root, 'computer-use', 'host.mjs')
    await mkdir(join(root, 'computer-use', 'bin', 'Open Harness Computer Use.app'), {
      recursive: true,
    })
    await writeFile(
      path,
      `import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);
      if (process.argv[2] !== '--request') process.exit(2);
      if (!process.env.OPENHARNESS_COMPUTER_APP?.endsWith('Open Harness Computer Use.app')) process.exit(3);
      const request = {id:1, method:process.argv[3]}; ${source}\n`,
      { mode: 0o755 },
    )
    plugins = new BundledPlugins({
      root,
      runtime,
      executable: process.execPath,
      browsers: join(directory, 'browsers'),
      platform: 'darwin',
      statusTimeoutMs,
    })
    return path
  }

  const computerPermissions = async () =>
    (await plugins.list()).find((item) => item.id === 'computer-use')?.permissions

  async function windowsHelper(result: Record<string, unknown>) {
    await mkdir(join(root, 'computer-use', 'windows', 'bin'), { recursive: true })
    await writeFile(
      join(root, 'computer-use', 'windows', 'bin', 'OpenHarnessComputerUse.exe'),
      'native helper fixture; the status proxy below verifies its environment',
    )
    for (const name of ['computer-use', 'browser-use'])
      await writeFile(join(root, name, 'launch.exe'), 'MCP launcher fixture')
    await writeFile(join(root, 'computer-use', 'server.mjs'), '// MCP server fixture')
    await writeFile(
      join(root, 'computer-use', 'host.mjs'),
      `import { existsSync } from 'node:fs';
      if (process.env.OPENHARNESS_COMPUTER_APP) process.exit(2);
      if (!existsSync(process.env.OPENHARNESS_COMPUTER_HELPER)) process.exit(3);
      if (process.argv.slice(2).join(' ') !== '--request status') process.exit(4);
      console.log(JSON.stringify({id:1,result:${JSON.stringify(result)}}));`,
    )
    plugins = new BundledPlugins({
      root,
      runtime,
      executable: process.execPath,
      browsers: join(directory, 'browsers'),
      platform: 'win32',
    })
  }

  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'openharness-bundled-plugins-'))
    root = join(directory, 'plugins')
    runtime = join(directory, 'runtime', 'index.cjs')
    browser = join(directory, 'browser-binary')
    setupMarker = join(directory, 'setup-calls')
    await mkdir(join(root, 'browser-use'), { recursive: true })
    await mkdir(join(directory, 'runtime'))
    await writeFile(join(root, 'browser-use', 'server.mjs'), '// host-owned plugin fixture\n')
    await writeFile(browser, 'not executed by a readiness check')
    await writeFile(
      runtime,
      `exports.chromium = { executablePath: () => ${JSON.stringify(browser)} };`,
    )
    await writeFile(
      join(directory, 'runtime', 'cli.js'),
      `
      if (process.argv.slice(2).join(' ') !== 'install chromium') process.exit(9);
      require('node:fs').appendFileSync(${JSON.stringify(setupMarker)}, ${JSON.stringify('prepared\n')});
      setTimeout(() => process.exit(0), 50);
    `,
    )
    plugins = new BundledPlugins({
      root,
      runtime,
      executable: process.execPath,
      browsers: join(directory, 'browsers'),
      platform: 'linux',
    })
  })

  afterEach(async () => {
    plugins.stop()
    await rm(directory, { recursive: true, force: true })
  })

  it('uses host-owned runtime paths and reports platform support without launching browser tools', async () => {
    expect(plugins.environment).toMatchObject({
      OPENHARNESS_NODE_BINARY: process.execPath,
      ELECTRON_RUN_AS_NODE: '1',
      OPENHARNESS_BROWSER_RUNTIME: runtime,
      PLAYWRIGHT_BROWSERS_PATH: join(directory, 'browsers'),
      OPENHARNESS_COMPUTER_APP: join(root, 'computer-use', 'bin', 'Open Harness Computer Use.app'),
    })
    const items = await plugins.list()
    expect(items.find((item) => item.id === 'browser-use')).toMatchObject({
      available: true,
      browserReady: true,
    })
    expect(items.find((item) => item.id === 'computer-use')).toMatchObject({
      available: false,
      reason: '目前支持 macOS 和 Windows。',
    })
    await expect(readFile(setupMarker)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects arbitrary IDs and paths before contacting the backend', async () => {
    const request = vi.fn()
    const backend = { request } as unknown as Backend
    for (const value of [
      null,
      '../browser-use',
      '/tmp/other-plugin',
      'https://example.com/plugin',
      'third-party',
      { id: 'browser-use', source_path: '/tmp/untrusted' },
    ]) {
      await expect(plugins.install(value, backend)).rejects.toThrow('未知的内置插件')
    }
    expect(request).not.toHaveBeenCalled()
  })

  it('keeps active Chrome clients and approvals intact when repairing an enabled bundled plugin', async () => {
    const request = vi.fn().mockResolvedValue({
      plugins: [
        {
          id: 'chrome-use',
          enabled: true,
          source_path: join(root, 'chrome-use'),
        },
      ],
    })
    const install = vi.spyOn(plugins, 'install')
    await plugins.enableChrome({ request } as unknown as Backend)
    expect(install).not.toHaveBeenCalled()
    expect(request.mock.calls).toEqual([[{ path: '/api/extensions', method: 'GET' }]])
  })

  it('only enables the existing bundled Chrome plugin and rejects an unrelated same-ID plugin', async () => {
    const installed = { id: 'chrome-use', enabled: false, source_path: join(root, 'chrome-use') }
    const request = vi.fn().mockResolvedValue({ plugins: [installed] })
    await plugins.enableChrome({ request } as unknown as Backend)
    expect(request).toHaveBeenLastCalledWith({
      path: '/api/plugins/chrome-use/enabled',
      method: 'POST',
      body: { enabled: true },
    })
    installed.source_path = '/other/chrome-use'
    request.mockClear()
    await expect(plugins.enableChrome({ request } as unknown as Backend)).rejects.toThrow(
      '其他位置',
    )
    expect(request).toHaveBeenCalledOnce()
  })

  it('installs the bundled Chrome plugin as part of first connection', async () => {
    const request = vi.fn().mockResolvedValue({ plugins: [] })
    const backend = { request } as unknown as Backend
    const install = vi.spyOn(plugins, 'install').mockResolvedValue({ plugins: [] } as never)
    await plugins.enableChrome(backend)
    expect(install).toHaveBeenCalledExactlyOnceWith('chrome-use', backend)
  })

  it('installs and updates only the fixed package path and never starts browser preparation implicitly', async () => {
    const settings = { plugins: [] as { id: string }[] }
    const request = vi.fn().mockResolvedValue(settings)
    const backend = { request } as unknown as Backend
    expect(await plugins.install('browser-use', backend)).toBe(settings)
    expect(request.mock.calls).toEqual([
      [{ path: '/api/extensions', method: 'GET' }],
      [
        {
          path: '/api/plugins/install',
          method: 'POST',
          body: { source_path: join(root, 'browser-use') },
        },
      ],
    ])
    request.mockClear()
    settings.plugins.push({ id: 'browser-use' })
    await plugins.install('browser-use', backend)
    expect(request).toHaveBeenLastCalledWith({
      path: '/api/plugins/install',
      method: 'POST',
      body: { source_path: join(root, 'browser-use'), expected_id: 'browser-use' },
    })
    await expect(readFile(setupMarker)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('requires an available packaged runtime and rejects unsupported platform installs', async () => {
    const request = vi.fn()
    const backend = { request } as unknown as Backend
    await expect(plugins.install('computer-use', backend)).rejects.toThrow(
      '目前支持 macOS 和 Windows',
    )
    await rm(runtime)
    expect((await plugins.list()).find((item) => item.id === 'browser-use')?.available).toBe(false)
    await expect(plugins.install('browser-use', backend)).rejects.toThrow('尚未包含')
    expect(request).not.toHaveBeenCalled()
  })

  it('coalesces explicit browser setup requests and executes only the host-managed installer', async () => {
    const first = plugins.prepareBrowser()
    const second = plugins.prepareBrowser()
    expect(first).toBe(second)
    await Promise.all([first, second])
    expect(await readFile(setupMarker, 'utf8')).toBe('prepared\n')
  })

  it('prepares the bundled browser on Windows and requires its native MCP launcher', async () => {
    await windowsHelper({ platform: 'windows', desktop_available: true })
    await rm(browser)
    const state = () =>
      plugins.list().then((items) => items.find((item) => item.id === 'browser-use'))
    expect(await state()).toMatchObject({ available: true, browserReady: false })
    await plugins.prepareBrowser()
    expect(await readFile(setupMarker, 'utf8')).toBe('prepared\n')
    await writeFile(browser, 'download completed fixture')
    expect(await state()).toMatchObject({ available: true, browserReady: true })
    await rm(join(root, 'browser-use', 'launch.exe'))
    expect(await state()).toMatchObject({ available: false, browserReady: false })
  })

  it.each([true, false])(
    'reports Windows desktop availability %s independently of macOS permissions',
    async (available) => {
      await windowsHelper({ platform: 'windows', desktop_available: available })
      const computer = (await plugins.list()).find((item) => item.id === 'computer-use')
      expect(computer).toMatchObject({
        available: true,
        desktop: {
          available,
          helperPath: join(root, 'computer-use', 'windows', 'bin', 'OpenHarnessComputerUse.exe'),
        },
      })
      expect(computer?.permissions).toBeUndefined()
      await expect(plugins.requestComputerPermission('accessibility')).rejects.toThrow(
        '无效的系统权限入口',
      )
    },
  )

  it.each([
    { platform: 'windows', desktop_available: 'true' },
    { platform: 'windows', accessibility: true, screen_recording: true },
    { platform: 'darwin', desktop_available: true },
  ])('does not report an invalid Windows status as unavailable: %#', async (result) => {
    await windowsHelper(result)
    expect((await plugins.list()).find((item) => item.id === 'computer-use')).toMatchObject({
      available: true,
      desktop: { available: null, error: expect.stringContaining('无效的桌面状态') },
    })
  })

  it('does not offer Windows computer use without both the helper and native MCP launcher', async () => {
    await windowsHelper({ platform: 'windows', desktop_available: true })
    await rm(join(root, 'computer-use', 'windows', 'bin', 'OpenHarnessComputerUse.exe'))
    expect((await plugins.list()).find((item) => item.id === 'computer-use')).toMatchObject({
      available: false,
      reason: '请重新构建桌面控制组件。',
    })
    const backend = { request: vi.fn() } as unknown as Backend
    await expect(plugins.install('computer-use', backend)).rejects.toThrow('请重新构建')
    expect(backend.request).not.toHaveBeenCalled()
    await windowsHelper({ platform: 'windows', desktop_available: true })
    await rm(join(root, 'computer-use', 'launch.exe'))
    expect((await plugins.list()).find((item) => item.id === 'computer-use')?.available).toBe(false)
  })

  it('rejects invalid or unsupported permission requests before invoking a native helper', async () => {
    for (const kind of ['accessibility', 'screen-recording'])
      await expect(plugins.requestComputerPermission(kind)).rejects.toThrow('无效的系统权限入口')
    const mac = new BundledPlugins({
      root,
      runtime,
      executable: process.execPath,
      browsers: join(directory, 'browsers'),
      platform: 'darwin',
    })
    for (const kind of [null, true, {}, [], 'status', 'screen_recording', '../helper', 'all'])
      await expect(mac.requestComputerPermission(kind)).rejects.toThrow('无效的系统权限入口')
    await expect(mac.requestComputerPermission('accessibility')).rejects.toThrow(
      '请重新构建桌面控制组件',
    )
    await expect(mac.requestComputerPermission('screen-recording')).rejects.toThrow(
      '请重新构建桌面控制组件',
    )
  })

  it.each([
    [true, true],
    [false, false],
    [true, false],
    [false, true],
  ])(
    'preserves explicit macOS permission values %s / %s',
    async (accessibility, screenRecording) => {
      await computerHelper(`
      if (request.id !== 1 || request.method !== 'status') process.exit(2);
      console.log(JSON.stringify({ id: request.id, result: {
        accessibility: ${accessibility}, screen_recording: ${screenRecording}
      } }));
    `)
      expect(await computerPermissions()).toEqual({
        accessibility,
        screenRecording,
        helperPath: join(root, 'computer-use', 'bin', 'Open Harness Computer Use.app'),
      })
    },
  )

  it('reports native errors without presenting unknown permissions as denied', async () => {
    await computerHelper(`
      const bytes = Buffer.from(JSON.stringify({id:1,error:{code:'NATIVE_ERROR',message:'权限检测未完成'}}));
      const split = bytes.indexOf(Buffer.from('权限')) + 1;
      process.stdout.write(bytes.subarray(0, split));
      setTimeout(() => process.stdout.write(bytes.subarray(split)), 10);
    `)
    expect(await computerPermissions()).toMatchObject({
      accessibility: null,
      screenRecording: null,
      error: '电脑权限检测失败：权限检测未完成（NATIVE_ERROR）',
    })
  })

  it.each([
    ['not JSON', '无效响应'],
    [
      JSON.stringify({ id: 2, result: { accessibility: true, screen_recording: true } }),
      '无效响应',
    ],
    [
      JSON.stringify({ id: '1', result: { accessibility: true, screen_recording: true } }),
      '无效响应',
    ],
    [JSON.stringify({ result: { accessibility: true, screen_recording: true } }), '无效响应'],
    [JSON.stringify({ id: 1, result: [] }), '无效响应'],
    [JSON.stringify({ id: 1, error: { message: 'missing code' } }), '无效响应'],
    [
      JSON.stringify({ id: 1, result: { accessibility: 'true', screen_recording: false } }),
      '无效的权限状态',
    ],
    [JSON.stringify({ id: 1, result: { accessibility: true } }), '无效的权限状态'],
    ['x'.repeat(16_385), '超过限制'],
  ])(
    'rejects malformed helper output instead of coercing permissions: %#',
    async (output, message) => {
      await computerHelper(`process.stdout.write(${JSON.stringify(output)});`)
      expect(await computerPermissions()).toMatchObject({
        accessibility: null,
        screenRecording: null,
        error: expect.stringContaining(message),
      })
    },
  )

  it('reports proxy and nonzero-exit failures separately from permissions', async () => {
    const path = await computerHelper('process.exit(7);')
    expect((await computerPermissions())?.error).toContain('异常退出（7）')
    await writeFile(path, 'invalid JavaScript fixture')
    expect(await computerPermissions()).toMatchObject({
      accessibility: null,
      screenRecording: null,
      error: expect.stringContaining('异常退出（1）'),
    })
  })

  it('times out a hung helper and forcibly cleans it up if it ignores termination', async () => {
    const pidFile = join(directory, 'helper-pid')
    await computerHelper(
      `
      process.on('SIGTERM', () => {});
      require('node:fs').writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));
      setInterval(() => {}, 1000);
    `,
      1_000,
    )
    expect(await computerPermissions()).toMatchObject({
      accessibility: null,
      screenRecording: null,
      error: expect.stringContaining('超时'),
    })
    const pid = Number(await readFile(pidFile, 'utf8'))
    await vi.waitFor(
      () => {
        expect(() => process.kill(pid, 0)).toThrow()
      },
      { timeout: 2_000 },
    )
  })

  it('settles permission checks and terminates helpers when the host stops', async () => {
    const pidFile = join(directory, 'helper-pid')
    await computerHelper(`
      require('node:fs').writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));
      setInterval(() => {}, 1000);
    `)
    const pending = computerPermissions()
    let pid = 0
    await vi.waitFor(async () => {
      pid = Number(await readFile(pidFile, 'utf8'))
      expect(pid).toBeGreaterThan(0)
    })
    plugins.stop()
    expect(await pending).toMatchObject({
      accessibility: null,
      screenRecording: null,
      error: expect.stringContaining('已停止'),
    })
    await vi.waitFor(() => expect(() => process.kill(pid, 0)).toThrow())
  })

  it('validates permission-request responses while accepting an explicit refusal', async () => {
    await computerHelper(`
      console.log(JSON.stringify({id:request.id,result:{
        permission:request.method === 'request_accessibility' ? 'accessibility' : 'screen_recording',
        granted:false
      }}));
    `)
    await expect(plugins.requestComputerPermission('accessibility')).resolves.toBeUndefined()
    await expect(plugins.requestComputerPermission('screen-recording')).resolves.toBeUndefined()
    await computerHelper(
      `console.log(JSON.stringify({id:1,result:{permission:'accessibility',granted:'true'}}));`,
    )
    await expect(plugins.requestComputerPermission('accessibility')).rejects.toThrow('无效结果')
    await computerHelper(
      `console.log(JSON.stringify({id:2,result:{permission:'accessibility',granted:true}}));`,
    )
    await expect(plugins.requestComputerPermission('accessibility')).rejects.toThrow('无效响应')
  })
})
