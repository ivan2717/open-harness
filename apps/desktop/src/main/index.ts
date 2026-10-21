import {
  app,
  BrowserWindow,
  ipcMain,
  session,
  type IpcMainEvent,
  type IpcMainInvokeEvent,
} from 'electron'
import { isAbsolute, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Backend } from './backend'
import { apiRequest, channels, identifier, watchRequest } from './protocol'
import { SubscriptionRegistry } from './subscriptions'

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
  ipcMain.handle(channels.invoke, async (event, request: unknown) => {
    assertSender(event)
    if (!backend) throw new Error('The local runtime is starting')
    return backend.request(apiRequest(request))
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
