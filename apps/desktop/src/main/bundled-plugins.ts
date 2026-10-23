import { spawn } from 'node:child_process'
import { access } from 'node:fs/promises'
import { join } from 'node:path'
import type { BundledPlugin, ExtensionSettings } from '../shared/extensions'
import type { Backend } from './backend'
import type { ChromeConnection } from './chrome-connection'

/** Only host-owned package paths and executables cross this bridge. */
export class BundledPlugins {
  private preparing: Promise<void> | undefined
  private child: ReturnType<typeof spawn> | undefined
  private helpers = new Map<ReturnType<typeof spawn>, () => void>()
  readonly environment: NodeJS.ProcessEnv

  constructor(
    private options: {
      root: string
      runtime: string
      executable: string
      browsers: string
      platform: NodeJS.Platform
      statusTimeoutMs?: number
      chrome?: ChromeConnection
    },
  ) {
    this.environment = {
      OPENHARNESS_BUNDLED_PLUGINS_DIR: options.root,
      OPENHARNESS_NODE_BINARY: options.executable,
      ELECTRON_RUN_AS_NODE: '1',
      OPENHARNESS_BROWSER_RUNTIME: options.runtime,
      PLAYWRIGHT_BROWSERS_PATH: options.browsers,
      DISPLAY: process.env.DISPLAY ?? '',
      WAYLAND_DISPLAY: process.env.WAYLAND_DISPLAY ?? '',
      XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? '',
      XAUTHORITY: process.env.XAUTHORITY ?? '',
      OPENHARNESS_COMPUTER_APP: join(
        options.root,
        'computer-use',
        'bin',
        'Open Harness Computer Use.app',
      ),
      ...(options.chrome ? { OPENHARNESS_CHROME_CONFIG: options.chrome.configPath } : {}),
    }
  }

  async list(): Promise<BundledPlugin[]> {
    const exists = async (path: string) =>
      access(path).then(
        () => true,
        () => false,
      )
    const browser =
      (await exists(join(this.options.root, 'browser-use', 'server.mjs'))) &&
      (await exists(this.options.runtime)) &&
      this.options.platform !== 'win32'
    const computer =
      this.options.platform === 'darwin' &&
      (await exists(this.environment.OPENHARNESS_COMPUTER_APP!)) &&
      (await exists(join(this.options.root, 'computer-use', 'host.mjs')))
    const chrome =
      this.options.platform !== 'win32' &&
      (await exists(join(this.options.root, 'chrome-use', 'extension', 'manifest.json'))) &&
      !!this.options.chrome
    const browserReady = browser && (await this.checkBrowser())
    return [
      {
        id: 'chrome-use',
        name: 'Chrome',
        description:
          '连接你正在使用的 Chrome，在独立标签页中操作，沿用登录状态，无需保持浏览器在前台。',
        available: chrome,
        ...(chrome
          ? { chrome: await this.options.chrome!.status() }
          : { reason: '当前安装包尚未包含此平台的 Chrome 连接组件。' }),
      },
      {
        id: 'browser-use',
        name: '独立浏览器',
        description: '使用独立浏览器读取网页、填写表单和检查页面，按需查看截图。',
        available: browser,
        browserReady,
        ...(!browser ? { reason: '当前安装包尚未包含此平台的浏览器运行环境。' } : {}),
      },
      {
        id: 'computer-use',
        name: '电脑操作',
        description: '在授权的 macOS 应用中读取界面、点击、输入和截图。',
        available: computer,
        ...(computer
          ? { permissions: await this.computerPermissions() }
          : {
              reason:
                this.options.platform === 'darwin'
                  ? '请重新构建桌面控制组件。'
                  : '目前支持 macOS。',
            }),
      },
    ]
  }

  async install(value: unknown, backend: Backend): Promise<ExtensionSettings> {
    if (value !== 'browser-use' && value !== 'computer-use' && value !== 'chrome-use')
      throw new Error('未知的内置插件')
    const item = (await this.list()).find((plugin) => plugin.id === value)!
    if (!item.available) throw new Error(item.reason)
    const settings = (await backend.request({
      path: '/api/extensions',
      method: 'GET',
    })) as ExtensionSettings
    return backend.request({
      path: '/api/plugins/install',
      method: 'POST',
      body: {
        source_path: join(this.options.root, value),
        ...(settings.plugins.some((plugin) => plugin.id === value) ? { expected_id: value } : {}),
      },
    }) as Promise<ExtensionSettings>
  }

  async enableChrome(backend: Backend): Promise<void> {
    const settings = (await backend.request({
      path: '/api/extensions',
      method: 'GET',
    })) as ExtensionSettings
    const installed = settings.plugins.find((plugin) => plugin.id === 'chrome-use')
    if (!installed) {
      await this.install('chrome-use', backend)
      return
    }
    if (installed.source_path !== join(this.options.root, 'chrome-use'))
      throw new Error('已有 Chrome 插件来自其他位置，请先在插件管理中更新为内置版本。')
    // Reinstalling revokes active MCP clients and conversation approvals. A
    // connection repair must leave an already enabled bundled plugin alone.
    if (!installed.enabled) {
      await backend.request({
        path: '/api/plugins/chrome-use/enabled',
        method: 'POST',
        body: { enabled: true },
      })
    }
  }

  private checkBrowser(): Promise<boolean> {
    return new Promise((resolve) => {
      const child = spawn(
        this.options.executable,
        [
          '-e',
          'const p=require(process.env.OPENHARNESS_BROWSER_RUNTIME);process.exit(require("node:fs").existsSync(p.chromium.executablePath())?0:1)',
        ],
        {
          env: { ...process.env, ...this.environment },
          stdio: 'ignore',
          windowsHide: true,
        },
      )
      const timer = setTimeout(() => {
        child.kill()
        resolve(false)
      }, 5_000)
      child.once('error', () => {
        clearTimeout(timer)
        resolve(false)
      })
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code === 0)
      })
    })
  }

  private async computerPermissions(): Promise<BundledPlugin['permissions']> {
    const helperPath = this.environment.OPENHARNESS_COMPUTER_APP!
    try {
      const result = await this.helperRequest('status', this.options.statusTimeoutMs ?? 20_000)
      if (typeof result.accessibility !== 'boolean' || typeof result.screen_recording !== 'boolean')
        throw new Error('电脑权限检测组件返回了无效的权限状态。')
      return {
        accessibility: result.accessibility,
        screenRecording: result.screen_recording,
        helperPath,
      }
    } catch (error) {
      return {
        accessibility: null,
        screenRecording: null,
        helperPath,
        error: error instanceof Error ? error.message : '电脑权限检测失败，请重新检查。',
      }
    }
  }

  async requestComputerPermission(kind: unknown): Promise<void> {
    if (
      this.options.platform !== 'darwin' ||
      (kind !== 'accessibility' && kind !== 'screen-recording')
    )
      throw new Error('无效的系统权限入口')
    await access(this.environment.OPENHARNESS_COMPUTER_APP!).catch(() => {
      throw new Error('请重新构建桌面控制组件。')
    })
    const result = await this.helperRequest(
      kind === 'accessibility' ? 'request_accessibility' : 'request_screen_recording',
      60_000,
    )
    if (
      typeof result.granted !== 'boolean' ||
      result.permission !== (kind === 'accessibility' ? 'accessibility' : 'screen_recording')
    )
      throw new Error('系统权限请求返回了无效结果，请在系统设置中检查权限。')
  }

  private helperRequest(
    method: 'status' | 'request_accessibility' | 'request_screen_recording',
    timeout: number,
  ): Promise<Record<string, unknown>> {
    return new Promise((resolve, reject) => {
      const operation = method === 'status' ? '电脑权限检测' : '系统权限请求'
      const child = spawn(
        this.options.executable,
        [join(this.options.root, 'computer-use', 'host.mjs'), '--request', method],
        {
          env: { ...process.env, ...this.environment },
          stdio: ['pipe', 'pipe', 'ignore'],
        },
      )
      let settled = false
      let closed = false
      let forceKill: ReturnType<typeof setTimeout> | undefined
      let output = ''
      const finish = (error?: Error, result?: Record<string, unknown>) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        if (error) {
          if (!closed) {
            child.stdin.destroy()
            child.stdout.destroy()
            child.kill('SIGTERM')
            // A stuck helper must not survive a timeout or application shutdown.
            if (child.pid && child.exitCode === null && child.signalCode === null) {
              forceKill = setTimeout(() => {
                if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
              }, 500)
              forceKill.unref()
            }
          }
          reject(error)
        } else resolve(result!)
      }
      const timer = setTimeout(() => {
        finish(new Error(`${operation}超时，请重新检查；这不代表系统未授权。`))
      }, timeout)
      this.helpers.set(child, () => finish(new Error(`${operation}已停止，请重新检查。`)))
      child.stdin.on('error', () => finish(new Error(`${operation}组件的输入连接中断。`)))
      child.stdout.on('error', () => finish(new Error(`${operation}组件的输出连接中断。`)))
      child.stdout.setEncoding('utf8')
      child.stdout.on('data', (data) => {
        if (settled) return
        output += String(data)
        if (Buffer.byteLength(output, 'utf8') > 16_384)
          finish(new Error(`${operation}组件返回的内容超过限制。`))
      })
      child.once('error', (error: NodeJS.ErrnoException) => {
        finish(new Error(`${operation}组件无法启动${error.code ? `（${error.code}）` : ''}。`))
      })
      // 'exit' can precede the last stdout chunk. Parse only once all pipes close.
      child.once('close', (code, signal) => {
        closed = true
        this.helpers.delete(child)
        clearTimeout(forceKill)
        if (settled) return
        if (code !== 0 || signal) {
          finish(new Error(`${operation}组件异常退出（${signal ?? code ?? '未知状态'}）。`))
          return
        }
        try {
          const response: unknown = JSON.parse(output.trim())
          if (
            !response ||
            typeof response !== 'object' ||
            Array.isArray(response) ||
            !('id' in response) ||
            response.id !== 1
          )
            throw new Error('invalid helper response')
          if ('error' in response) {
            const error = response.error
            if (
              !error ||
              typeof error !== 'object' ||
              !('code' in error) ||
              typeof error.code !== 'string' ||
              !('message' in error) ||
              typeof error.message !== 'string'
            )
              throw new Error('invalid helper error')
            finish(
              new Error(
                `${operation}失败：${error.message.slice(0, 500)}（${error.code.slice(0, 80)}）`,
              ),
            )
            return
          }
          if (
            !('result' in response) ||
            !response.result ||
            typeof response.result !== 'object' ||
            Array.isArray(response.result)
          )
            throw new Error('invalid helper result')
          finish(undefined, response.result as Record<string, unknown>)
        } catch {
          finish(new Error(`${operation}组件返回了无效响应，请重新检查。`))
        }
      })
      child.stdin.end()
    })
  }

  prepareBrowser(): Promise<void> {
    if (this.preparing) return this.preparing
    const task = new Promise<void>((resolve, reject) => {
      const child = spawn(
        this.options.executable,
        [join(this.options.runtime, '..', 'cli.js'), 'install', 'chromium'],
        {
          env: { ...process.env, ...this.environment },
          stdio: 'ignore',
          windowsHide: true,
        },
      )
      this.child = child
      const timer = setTimeout(() => {
        child.kill()
        reject(new Error('浏览器下载超时，请检查网络后重试。'))
      }, 5 * 60_000)
      child.once('error', () => {
        clearTimeout(timer)
        reject(new Error('无法启动浏览器安装程序。'))
      })
      child.once('exit', (code) => {
        clearTimeout(timer)
        this.child = undefined
        if (code === 0) resolve()
        else reject(new Error('浏览器未准备完成，请检查网络后重试。'))
      })
    })
    this.preparing = task.finally(() => {
      this.preparing = undefined
    })
    return this.preparing
  }

  stop(): void {
    this.child?.kill()
    for (const cancel of this.helpers.values()) cancel()
  }
}
