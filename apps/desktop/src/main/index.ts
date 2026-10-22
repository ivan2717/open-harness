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
import { realpath, mkdir } from 'node:fs/promises'
import { Backend } from './backend'
import {
  apiRequest,
  channels,
  identifier,
  resolvedModelConfiguration,
  watchRequest,
} from './protocol'
import { SubscriptionRegistry } from './subscriptions'
import { ModelVault } from './model-vault'
import { fetchModelCapacity } from './model-metadata'
import { resolveModelLimits } from '../shared/model-limits'
import { importClipboardFiles, importFilePaths } from './file-references'
import {
  installMessageResourceBridge,
  registerMessageResourceScheme,
} from './message-resource-bridge'
import type { AppConfig, ConfigureModelInput, ModelInfo } from '../shared/contracts'

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
let quitting = false
let modelVault: ModelVault | undefined
let modelsReady: Promise<void> = Promise.resolve()
let modelConfigError: string | undefined
// A renderer can only start work in directories selected through the native dialog.
const selectedWorkspaces = new Set<string>()

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
    return importFilePaths(result.canceled ? [] : result.filePaths)
  })
  ipcMain.handle(channels.importFiles, async (event, paths: unknown) => {
    assertSender(event)
    return importFilePaths(paths)
  })
  ipcMain.handle(channels.pasteFiles, async (event) => {
    assertSender(event)
    return importClipboardFiles(() => clipboard.read())
  })
  ipcMain.handle(channels.chooseWorkspace, async (event) => {
    assertSender(event)
    const result = await dialog.showOpenDialog(window!, {
      title: '选择项目目录',
      buttonLabel: '选择项目',
      properties: ['openDirectory'],
    })
    if (result.canceled || !result.filePaths[0]) return null
    const path = await realpath(result.filePaths[0])
    selectedWorkspaces.add(path)
    return path
  })
  ipcMain.handle(channels.invoke, async (event, request: unknown) => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    const mapped = apiRequest(request)
    if (
      mapped.path === '/api/runs' &&
      mapped.method === 'POST' &&
      mapped.body &&
      'workspace_path' in mapped.body
    ) {
      if (!mapped.body.workspace_path || !selectedWorkspaces.has(mapped.body.workspace_path))
        throw new Error('请先通过“选择项目”授权此目录')
    }
    await modelsReady
    if (mapped.path === '/api/models') {
      const input = mapped.body as ConfigureModelInput
      const capacity = await fetchModelCapacity(input)
      const limits = resolveModelLimits(input, capacity)
      const resolved = resolvedModelConfiguration({
        ...input,
        context_window: limits.context_window,
        max_output_tokens: limits.max_output_tokens,
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
      void backend.watch(
        id,
        controller.signal,
        (snapshot) => send({ snapshot }),
        (error) => send({ error }),
      )
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
  void app.whenReady().then(() => {
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
    backend = new Backend(binary, join(app.getPath('userData'), 'runtime'))
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
    modelsReady = (async () => {
      try {
        for (const input of await modelVault!.load()) {
          try {
            await backend!.request({ path: '/api/models', method: 'POST', body: input })
          } catch {
            modelConfigError = '部分模型配置未能恢复，可重新接入对应模型。'
          }
        }
      } catch {
        modelConfigError = '已保存的模型配置未能恢复。可在这里重新接入；本地会话记录仍然可用。'
      }
    })()
    void backend.start().catch(() => undefined) // Bridge calls surface startup failures in the workbench.
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
    subscriptions.stopAll()
    void (backend?.stop() ?? Promise.resolve()).finally(() => app.quit())
  })
}
