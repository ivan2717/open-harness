import {
  app,
  BrowserWindow,
  ipcMain,
  session,
  safeStorage,
  dialog,
  shell,
  clipboard,
  type IpcMainEvent,
  type IpcMainInvokeEvent,
} from 'electron'
import { isAbsolute, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdir } from 'node:fs/promises'
import { Backend } from './backend'
import { initializeRuntime, watchInitializedRuntime } from './runtime-startup'
import { prepareWorkspaceRuntime } from './workspace-runtime'
import { BundledPlugins } from './bundled-plugins'
import { ChromeConnection } from './chrome-connection'
import { COMPUTER_APP_NAME, installComputerApp } from './computer-app'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { createRequire } from 'node:module'
import { installPluginFromDialog } from './plugin-install'
import { WorkspaceSelection } from './workspace-selection'
import { revealRunWorkspace } from './run-workspace'
import {
  apiRequest,
  channels,
  identifier,
  resolvedModelConfiguration,
  watchRequest,
} from './protocol'
import { SubscriptionRegistry } from './subscriptions'
import { ModelVault } from './model-vault'
import { fetchModelMetadata } from './model-metadata'
import { resolveReasoningCapability } from '../shared/model-reasoning'
import { refreshModelContext } from './model-context'
import { getModelConfiguration, updateModelConfiguration } from './model-edit'
import { resolveModelLimits } from '../shared/model-limits'
import { importClipboardFiles, importFilePaths } from './file-references'
import { importImages } from './image-references'
import {
  installMessageResourceBridge,
  registerMessageResourceScheme,
} from './message-resource-bridge'
import type {
  AppConfig,
  ConfigureModelInput,
  ModelInfo,
  RunSummary,
  UpdateModelInput,
} from '../shared/contracts'

const dataDirectory = process.env.OPENHARNESS_DESKTOP_DATA_DIR
if (dataDirectory) {
  if (!isAbsolute(dataDirectory))
    throw new Error('OPENHARNESS_DESKTOP_DATA_DIR must be an absolute path')
  app.setPath('userData', dataDirectory)
}

const rendererPath = resolve(__dirname, '../renderer/index.html')
const devUrl = !app.isPackaged ? process.env.ELECTRON_RENDERER_URL : undefined
const subscriptions = new SubscriptionRegistry()
let window: BrowserWindow | undefined
let backend: Backend | undefined
let bundledPlugins: BundledPlugins | undefined
let chromeConnection: ChromeConnection | undefined
let quitting = false
let modelVault: ModelVault | undefined
let modelsReady: Promise<void> = Promise.resolve()
let modelConfigError: string | undefined
const workspaces = new WorkspaceSelection(
  join(app.getPath('userData'), 'workspaces.json'),
  async () => {
    if (!backend) throw new Error('The local runtime is starting')
    return (await backend.request({ path: '/api/runs', method: 'GET' })) as RunSummary[]
  },
)

registerMessageResourceScheme()

function trustedUrl(url: string): boolean {
  try {
    const parsed = new URL(url)
    if (devUrl) return parsed.origin === new URL(devUrl).origin
    return parsed.protocol === 'file:' && fileURLToPath(parsed) === rendererPath
  } catch {
    return false
  }
}

function assertSender(event: IpcMainInvokeEvent | IpcMainEvent): void {
  if (
    !window ||
    window.isDestroyed() ||
    event.sender.id !== window.webContents.id ||
    event.senderFrame !== event.sender.mainFrame ||
    !trustedUrl(event.senderFrame.url)
  ) {
    throw new Error('Untrusted desktop request')
  }
}

function installBridge(): void {
  const importAttachment = (name: string, bytes: Buffer) => {
    if (!backend) throw new Error('本地服务正在启动，请稍后添加。')
    return backend.importAttachment(name, bytes)
  }
  ipcMain.handle(channels.bundledPlugins, async (event) => {
    assertSender(event)
    return bundledPlugins!.list()
  })
  ipcMain.handle(channels.installBundledPlugin, async (event, id: unknown) => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    return bundledPlugins!.install(id, backend)
  })
  ipcMain.handle(channels.prepareBrowser, async (event) => {
    assertSender(event)
    return bundledPlugins!.prepareBrowser()
  })
  ipcMain.handle(channels.chromeConnectionStatus, async (event) => {
    assertSender(event)
    return chromeConnection!.status()
  })
  ipcMain.handle(channels.connectChrome, async (event, mode: unknown = 'store') => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    await chromeConnection!.connect(mode, {
      enablePlugin: () => bundledPlugins!.enableChrome(backend!),
      openDirectory: async (path) => {
        const error = await shell.openPath(path)
        if (error) throw new Error('无法打开扩展目录，请在开发者选项中查看目录路径。')
      },
      openChrome: async (url) => {
        if (process.platform === 'darwin') {
          await promisify(execFile)('/usr/bin/open', ['-a', 'Google Chrome', url], {
            timeout: 10_000,
          })
        } else if (process.platform === 'win32') {
          await promisify(execFile)(
            process.execPath,
            [
              join(
                app.isPackaged
                  ? join(process.resourcesPath, 'plugins')
                  : join(app.getAppPath(), 'resources/plugins'),
                'chrome-use',
                'open-url.mjs',
              ),
              url,
            ],
            {
              env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' },
              timeout: 20_000,
              windowsHide: true,
            },
          )
        } else {
          await new Promise<void>((resolve, reject) => {
            const chrome = spawn('google-chrome', [url], { detached: true, stdio: 'ignore' })
            chrome.once('error', reject)
            chrome.once('spawn', () => {
              chrome.unref()
              resolve()
            })
          })
        }
      },
    })
  })
  ipcMain.handle(channels.computerSettings, async (event, permission: unknown) => {
    assertSender(event)
    if (
      process.platform !== 'darwin' ||
      !['accessibility', 'screen-recording'].includes(String(permission))
    )
      throw new Error('无效的系统设置入口')
    try {
      // Invoked only by the explicit settings button, never by model tools or
      // a background permission check.
      await bundledPlugins!.requestComputerPermission(permission)
    } finally {
      await shell.openExternal(
        `x-apple.systempreferences:com.apple.preference.security?${permission === 'accessibility' ? 'Privacy_Accessibility' : 'Privacy_ScreenCapture'}`,
      )
    }
  })
  ipcMain.handle(
    channels.installPlugin,
    async (event, expectedId: unknown, sourceKind: unknown) => {
      assertSender(event)
      if (!backend) throw new Error('The local runtime is starting')
      return installPluginFromDialog(expectedId, sourceKind, {
        choose: (options) => dialog.showOpenDialog(window!, options),
        request: (request) => backend!.request(request),
      })
    },
  )
  ipcMain.handle(channels.openSkillsDirectory, async (event) => {
    assertSender(event)
    const directory = join(app.getPath('userData'), 'runtime', 'skills')
    await mkdir(directory, { recursive: true })
    const error = await shell.openPath(directory)
    if (error) throw new Error(error)
  })
  ipcMain.handle(channels.copyText, (event, text: unknown) => {
    assertSender(event)
    if (typeof text !== 'string' || Buffer.byteLength(text, 'utf8') > 2 * 1024 * 1024)
      throw new Error('复制内容过大，请分段选择复制。')
    clipboard.writeText(text)
  })
  ipcMain.handle(channels.chooseFiles, async (event) => {
    assertSender(event)
    const result = await dialog.showOpenDialog(window!, {
      title: '添加文件引用',
      buttonLabel: '添加引用',
      properties: ['openFile', 'multiSelections'],
    })
    return importFilePaths(result.canceled ? [] : result.filePaths, importAttachment)
  })
  ipcMain.handle(channels.importFiles, async (event, paths: unknown) => {
    assertSender(event)
    return importFilePaths(paths, importAttachment)
  })
  ipcMain.handle(channels.importImages, (event, images: unknown) => {
    assertSender(event)
    return importImages(images)
  })
  ipcMain.handle(channels.pasteFiles, async (event, selection: unknown) => {
    assertSender(event)
    return importClipboardFiles(() => clipboard.read(), importAttachment, selection)
  })
  ipcMain.handle(channels.chooseWorkspace, async (event) => {
    assertSender(event)
    return workspaces.choose((options) => dialog.showOpenDialog(window!, options))
  })
  ipcMain.handle(channels.openWorkspace, async (event, path: unknown) => {
    assertSender(event)
    return workspaces.open(path)
  })
  ipcMain.handle(channels.revealWorkspace, async (event, id: unknown) => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    return revealRunWorkspace(
      id,
      (request) => backend!.request(request),
      (path) => shell.openPath(path),
    )
  })
  ipcMain.handle(channels.invoke, async (event, request: unknown) => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    const mapped = apiRequest(request)
    if (mapped.workspacePath !== undefined) await workspaces.verify(mapped.workspacePath)
    if (
      mapped.path === '/api/runs' &&
      mapped.method === 'POST' &&
      mapped.body &&
      'workspace_path' in mapped.body
    ) {
      await workspaces.verify(mapped.body.workspace_path)
    }
    await modelsReady
    const modelConfigurationRoute = /^\/api\/models\/([a-zA-Z0-9_-]+)\/configuration$/.exec(
      mapped.path,
    )
    if (modelConfigurationRoute) {
      if (!modelVault) throw new Error('模型配置尚未就绪')
      return mapped.method === 'GET'
        ? getModelConfiguration(modelConfigurationRoute[1], modelVault)
        : updateModelConfiguration(
            modelConfigurationRoute[1],
            mapped.body as UpdateModelInput,
            backend,
            modelVault,
          )
    }
    const refreshContext = /^\/api\/models\/([a-zA-Z0-9_-]+)\/context\/refresh$/.exec(mapped.path)
    if (refreshContext) {
      if (!modelVault) throw new Error('模型配置尚未就绪')
      return refreshModelContext(refreshContext[1], backend, modelVault)
    }
    if (mapped.path === '/api/models') {
      const input = mapped.body as ConfigureModelInput
      const metadata = await fetchModelMetadata(input)
      const capacity = metadata?.capacity
      const limits = resolveModelLimits(input, capacity)
      const resolved = resolvedModelConfiguration({
        ...input,
        context_window: limits.context_window,
        max_output_tokens: limits.max_output_tokens,
        ...(input.reasoning_mode === 'off'
          ? { reasoning_capability: resolveReasoningCapability(input, metadata?.reasoning) }
          : {}),
      })
      const model = (await backend.request({ ...mapped, body: resolved })) as ModelInfo
      const persisted = (await modelVault?.save(model.id, resolved)) ?? false
      return { model, persisted, limits, capacity }
    }
    const result = await backend.request(mapped)
    if (mapped.path === '/api/config')
      return { ...(result as AppConfig), model_config_error: modelConfigError }
    return result
  })
  ipcMain.on(channels.watchStart, (event, request: unknown) => {
    try {
      assertSender(event)
      const { id, token } = watchRequest(request)
      const controller = subscriptions.start(event.sender.id, token)
      const send = (payload: object): void => {
        if (!controller.signal.aborted && !event.sender.isDestroyed())
          event.sender.send(channels.watchEvent, { token, ...payload })
      }
      if (!backend) {
        send({ error: 'The local runtime is starting' })
        subscriptions.stop(event.sender.id, token)
        return
      }
      const runtime = backend
      void watchInitializedRuntime(modelsReady, controller.signal, () =>
        runtime.watch(
          id,
          controller.signal,
          (snapshot) => send({ snapshot }),
          (error) => send({ error }),
        ),
      ).catch(() => send({ error: '本地服务初始化失败。请重新启动 Open Harness。' }))
    } catch {
      /* Invalid or unauthorized subscriptions have no side effects. */
    }
  })
  ipcMain.on(channels.watchStop, (event, token: unknown) => {
    try {
      assertSender(event)
      subscriptions.stop(event.sender.id, identifier(token))
    } catch {
      /* Reject invalid senders. */
    }
  })
}

function createWindow(): void {
  window = new BrowserWindow({
    width: 1440,
    height: 940,
    minWidth: 1000,
    minHeight: 680,
    title: 'Open Harness',
    backgroundColor: '#f8f8f5',
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
    webPreferences: {
      preload: join(__dirname, '../preload/index.cjs'),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true,
      allowRunningInsecureContent: false,
    },
  })
  const contents = window.webContents
  contents.setWindowOpenHandler(() => ({ action: 'deny' }))
  contents.on('will-navigate', (event) => event.preventDefault())
  contents.on('will-attach-webview', (event) => event.preventDefault())
  contents.on('render-process-gone', () => subscriptions.stopOwner(contents.id))
  contents.on('did-start-navigation', (_event, _url, isInPlace, isMainFrame) => {
    if (isMainFrame && !isInPlace) subscriptions.stopOwner(contents.id)
  })
  contents.once('destroyed', () => subscriptions.stopOwner(contents.id))
  window.on('closed', () => {
    window = undefined
  })
  if (devUrl) void window.loadURL(devUrl)
  else void window.loadFile(rendererPath)
}

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  app.on('second-instance', () => {
    if (window) {
      if (window.isMinimized()) window.restore()
      window.focus()
    } else if (app.isReady()) createWindow()
  })
  void app.whenReady().then(async () => {
    session.defaultSession.setPermissionRequestHandler((_contents, _permission, callback) =>
      callback(false),
    )
    session.defaultSession.setPermissionCheckHandler(() => false)
    const binaryName = process.platform === 'win32' ? 'api.exe' : 'api'
    const workspace = resolve(app.getAppPath(), '../..')
    const targetDirectory = process.env.CARGO_TARGET_DIR
      ? resolve(workspace, process.env.CARGO_TARGET_DIR)
      : join(workspace, 'target')
    const binary = app.isPackaged
      ? join(process.resourcesPath, 'bin', binaryName)
      : join(targetDirectory, 'debug', binaryName)
    const pluginRoot = app.isPackaged
      ? join(process.resourcesPath, 'plugins')
      : process.platform === 'win32'
        ? join(app.getAppPath(), 'resources/plugins')
        : join(workspace, 'plugins')
    let computerAppPath: string | undefined
    let computerSetupError: string | undefined
    if (process.platform === 'darwin') {
      try {
        computerAppPath = await installComputerApp(
          join(pluginRoot, 'computer-use', 'bin', COMPUTER_APP_NAME),
          app.getPath('userData'),
        )
      } catch {
        // Do not let a failed update launch an older or nested permission host.
        computerAppPath = join(app.getPath('userData'), 'computer-use', '.unavailable')
        computerSetupError = '电脑组件安装失败，请重启 Open Harness 或重新安装应用。'
        console.warn(computerSetupError)
      }
    }
    chromeConnection = new ChromeConnection({
      root: pluginRoot,
      executable: process.execPath,
      userData: app.getPath('userData'),
      home: app.getPath('home'),
      platform: process.platform,
    })
    void chromeConnection.refreshExisting().catch(() => {
      // Do not include filesystem/configuration details in startup logs.
      console.warn('Chrome 连接组件更新失败；可在扩展页重新配置，不影响应用启动。')
    })
    bundledPlugins = new BundledPlugins({
      root: pluginRoot,
      runtime: app.isPackaged
        ? join(process.resourcesPath, 'browser-runtime', 'index.js')
        : createRequire(__filename).resolve('playwright-core'),
      executable: process.execPath,
      browsers: process.env.PLAYWRIGHT_BROWSERS_PATH ?? join(app.getPath('userData'), 'browsers'),
      platform: process.platform,
      chrome: chromeConnection,
      computerAppPath,
      computerSetupError,
    })
    backend = new Backend(binary, join(app.getPath('userData'), 'runtime'), {
      ...bundledPlugins.environment,
      ...(await prepareWorkspaceRuntime({
        directory: join(app.getPath('userData'), 'workspace-runtime'),
        platform: process.platform,
        resourcesDirectory: app.isPackaged
          ? process.resourcesPath
          : join(app.getAppPath(), 'resources'),
      })),
    })
    installMessageResourceBridge({
      directory: app.getPath('userData'),
      backend,
      window: () => window!,
      assertSender,
    })
    modelVault = new ModelVault(join(app.getPath('userData'), 'models.encrypted'), {
      isEncryptionAvailable: () =>
        safeStorage.isEncryptionAvailable() &&
        (process.platform !== 'linux' || safeStorage.getSelectedStorageBackend() !== 'basic_text'),
      encryptString: (value) => safeStorage.encryptString(value),
      decryptString: (value) => safeStorage.decryptString(value),
    })
    modelsReady = initializeRuntime(
      backend,
      () => modelVault!.load(),
      (message) => {
        modelConfigError = message
      },
    ).catch(() => undefined) // Bridge calls surface the original startup failure.
    installBridge()
    createWindow()
    app.on('activate', () => {
      if (BrowserWindow.getAllWindows().length === 0) createWindow()
    })
  })
  app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') app.quit()
  })
  app.on('before-quit', (event) => {
    if (quitting) return
    event.preventDefault()
    quitting = true
    bundledPlugins?.stop()
    subscriptions.stopAll()
    void (backend?.stop() ?? Promise.resolve()).finally(() => app.quit())
  })
}
