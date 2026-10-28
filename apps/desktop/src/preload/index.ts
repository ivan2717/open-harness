import { contextBridge, ipcRenderer, webUtils, type IpcRendererEvent } from 'electron'
import type {
  AppConfig,
  ConfigureModelResult,
  DesktopBridge,
  RunDetail,
  RunSummary,
  WorkspaceReview,
  FileImportResult,
  ModelConfiguration,
} from '../shared/contracts'
import { channels, type BridgeRequest } from '../main/protocol'
import {
  MAX_REFERENCE_FILES,
  MAX_IMAGE_REFERENCE_FILES,
  MAX_IMAGE_SOURCE_BYTES,
} from '../shared/file-references'
import type { ResolvedResource } from '../shared/message-resources'
import type { RunMemory } from '../shared/memory'
import type {
  ExtensionSettings,
  RunExtensions,
  McpCatalog,
  SkillCatalog,
} from '../shared/extensions'

const invoke = <T>(request: BridgeRequest): Promise<T> =>
  ipcRenderer.invoke(channels.invoke, request) as Promise<T>
let nextSubscription = 0

async function importedImages(files: File[]): Promise<FileImportResult> {
  if (files.length > MAX_IMAGE_REFERENCE_FILES) throw new Error('每条消息最多添加 4 张图片。')
  const images = []
  const errors: string[] = []
  for (const file of files) {
    const name = file.name || '截图.png'
    try {
      if (!file.size || file.size > MAX_IMAGE_SOURCE_BYTES)
        throw new Error('图片超过 32 MiB 或内容为空。')
      images.push({ name, bytes: new Uint8Array(await file.arrayBuffer()) })
    } catch (error) {
      errors.push(`${name}：${error instanceof Error ? error.message : '无法读取图片。'}`)
    }
  }
  const result = images.length
    ? ((await ipcRenderer.invoke(channels.importImages, images)) as FileImportResult)
    : { files: [], errors: [] }
  return { files: result.files, errors: [...errors, ...result.errors] }
}

const bridge: DesktopBridge = {
  installPlugin: (expectedId, sourceKind = 'directory') =>
    ipcRenderer.invoke(
      channels.installPlugin,
      expectedId,
      sourceKind,
    ) as Promise<ExtensionSettings | null>,
  setPluginEnabled: (id, enabled) =>
    invoke<ExtensionSettings>({ operation: 'setPluginEnabled', id, enabled }),
  uninstallPlugin: (id) => invoke<ExtensionSettings>({ operation: 'uninstallPlugin', id }),
  extensionSettings: () => invoke<ExtensionSettings>({ operation: 'extensionSettings' }),
  skillCatalog: (workspacePath) =>
    invoke<SkillCatalog>({ operation: 'skillCatalog', workspacePath }),
  saveExtensions: (config) => invoke<ExtensionSettings>({ operation: 'saveExtensions', config }),
  testMcp: (id) => invoke<McpCatalog>({ operation: 'testMcp', id }),
  runExtensions: (id) => invoke<RunExtensions>({ operation: 'runExtensions', id }),
  runMemory: (id) => invoke<RunMemory>({ operation: 'runMemory', id }),
  refreshRunSkills: (id) => invoke<RunExtensions>({ operation: 'refreshRunSkills', id }),
  approveExtension: (id, approvalId, decision) =>
    invoke<RunExtensions>({ operation: 'approveExtension', id, approvalId, decision }),
  revokeControl: (id) => invoke<RunExtensions>({ operation: 'revokeControl', id }),
  revokeControlPermission: (serverId) =>
    invoke<ExtensionSettings>({ operation: 'revokeControlPermission', serverId }),
  bundledPlugins: () => ipcRenderer.invoke(channels.bundledPlugins),
  installBundledPlugin: (id) => ipcRenderer.invoke(channels.installBundledPlugin, id),
  prepareBrowser: () => ipcRenderer.invoke(channels.prepareBrowser),
  chromeConnectionStatus: () => ipcRenderer.invoke(channels.chromeConnectionStatus),
  connectChrome: (mode = 'store') => ipcRenderer.invoke(channels.connectChrome, mode),
  openComputerSettings: (permission) => ipcRenderer.invoke(channels.computerSettings, permission),
  openSkillsDirectory: () => ipcRenderer.invoke(channels.openSkillsDirectory) as Promise<void>,
  resolveResource: (reference) =>
    ipcRenderer.invoke(channels.resolveResource, reference) as Promise<ResolvedResource>,
  resourceAction: (id, action) =>
    ipcRenderer.invoke(channels.resourceAction, id, action) as Promise<void>,
  openExternal: (url) => ipcRenderer.invoke(channels.openExternal, url) as Promise<void>,
  copyText: (text) => ipcRenderer.invoke(channels.copyText, text) as Promise<void>,
  config: () => invoke<AppConfig>({ operation: 'config' }),
  listRuns: () => invoke<RunSummary[]>({ operation: 'listRuns' }),
  getRun: (id) => invoke<RunDetail>({ operation: 'getRun', id }),
  createRun: (input) => invoke<RunDetail>({ operation: 'createRun', input }),
  updateRunModel: (id, input) => invoke<RunDetail>({ operation: 'updateRunModel', id, input }),
  chooseWorkspace: () => ipcRenderer.invoke(channels.chooseWorkspace) as Promise<string | null>,
  openWorkspace: (path) => ipcRenderer.invoke(channels.openWorkspace, path) as Promise<string>,
  revealWorkspace: (id) => ipcRenderer.invoke(channels.revealWorkspace, id) as Promise<void>,
  chooseFiles: () => ipcRenderer.invoke(channels.chooseFiles) as Promise<FileImportResult>,
  importFiles: (files) => {
    if (!Array.isArray(files) || files.length > MAX_REFERENCE_FILES)
      return Promise.reject(new Error('每次最多添加 64 个文件。'))
    return ipcRenderer.invoke(
      channels.importFiles,
      files.map((file) => webUtils.getPathForFile(file)),
    ) as Promise<FileImportResult>
  },
  pasteFiles: async (files = []) => {
    if (!Array.isArray(files) || files.length > MAX_REFERENCE_FILES)
      throw new Error('每次最多添加 64 个文件。')
    const images = files.filter((file) => /^image\//i.test(file.type))
    const selection = files.length
      ? {
          files: files.map((file) => ({
            name: file.name,
            size: file.size,
            image: /^image\//i.test(file.type),
          })),
          skipImages: images.length > 0,
        }
      : undefined
    // Clipboard screenshots are memory Files. Never require a disk path for them.
    if (images.length) {
      const result = await importedImages(images)
      const otherFiles = files.filter((file) => !/^image\//i.test(file.type))
      if (otherFiles.length) {
        const native = (await ipcRenderer.invoke(
          channels.pasteFiles,
          selection,
        )) as FileImportResult
        const other =
          native.files.length || native.errors.length
            ? native
            : await bridge.importFiles(otherFiles)
        result.files.push(...other.files)
        result.errors.push(...other.errors)
      }
      return result
    }
    const native = (await ipcRenderer.invoke(channels.pasteFiles, selection)) as FileImportResult
    if (native.files.length || native.errors.length || !files.length) return native
    return bridge.importFiles(files)
  },
  getWorkspace: (id) => invoke<WorkspaceReview>({ operation: 'getWorkspace', id }),
  approveCommand: (id, approvalId, allow, scope) =>
    invoke<WorkspaceReview>({ operation: 'approveCommand', id, approvalId, allow, scope }),
  revokeCommandGrants: (id) => invoke<WorkspaceReview>({ operation: 'revokeCommandGrants', id }),
  stopCommand: (id, commandId) =>
    invoke<WorkspaceReview>({ operation: 'stopCommand', id, commandId }),
  revertFile: (id, path) => invoke<WorkspaceReview>({ operation: 'revertFile', id, path }),
  resumeRun: (id, message, files) =>
    invoke<RunDetail>({ operation: 'resumeRun', id, message, files }),
  cancelRun: (id) => invoke<RunDetail>({ operation: 'cancelRun', id }),
  compactRun: (id) => invoke<RunDetail>({ operation: 'compactRun', id }),
  cancelCompaction: (id) => invoke<RunDetail>({ operation: 'cancelCompaction', id }),
  configureModel: (input) => invoke<ConfigureModelResult>({ operation: 'configureModel', input }),
  getModelConfiguration: (id) =>
    invoke<ModelConfiguration>({ operation: 'getModelConfiguration', id }),
  updateModelConfiguration: (id, input) =>
    invoke<ConfigureModelResult>({ operation: 'updateModelConfiguration', id, input }),
  refreshModelContext: (id) =>
    invoke<ConfigureModelResult>({ operation: 'refreshModelContext', id }),
  submitInput: (id, message, files) =>
    invoke<RunDetail>({ operation: 'submitInput', id, message, files }),
  watchRun: (id, listener, onError) => {
    const token = `${Date.now()}-${nextSubscription++}-${Math.random().toString(36).slice(2)}`
    let active = true
    const receive = (
      _event: IpcRendererEvent,
      payload: { token?: string; snapshot?: RunDetail; error?: string },
    ): void => {
      if (!active || payload.token !== token) return
      if (payload.snapshot) listener(payload.snapshot)
      else if (payload.error) onError(payload.error)
    }
    ipcRenderer.on(channels.watchEvent, receive)
    ipcRenderer.send(channels.watchStart, { id, token })
    return () => {
      if (!active) return
      active = false
      ipcRenderer.removeListener(channels.watchEvent, receive)
      ipcRenderer.send(channels.watchStop, token)
    }
  },
  platform: process.platform,
}

contextBridge.exposeInMainWorld('harness', bridge)
