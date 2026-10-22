import { contextBridge, ipcRenderer, webUtils, type IpcRendererEvent } from 'electron'
import type {
  AppConfig,
  ConfigureModelResult,
  DesktopBridge,
  RunDetail,
  RunSummary,
  WorkspaceReview,
  FileImportResult,
} from '../shared/contracts'
import { channels, type BridgeRequest } from '../main/protocol'
import { MAX_REFERENCE_FILES } from '../shared/file-references'
import type { ResolvedResource } from '../shared/message-resources'
import type { ExtensionSettings, RunExtensions, McpCatalog } from '../shared/extensions'

const invoke = <T>(request: BridgeRequest): Promise<T> =>
  ipcRenderer.invoke(channels.invoke, request) as Promise<T>
let nextSubscription = 0

const bridge: DesktopBridge = {
  extensionSettings: () => invoke<ExtensionSettings>({ operation: 'extensionSettings' }),
  saveExtensions: (config) => invoke<ExtensionSettings>({ operation: 'saveExtensions', config }),
  testMcp: (id) => invoke<McpCatalog>({ operation: 'testMcp', id }),
  runExtensions: (id) => invoke<RunExtensions>({ operation: 'runExtensions', id }),
  refreshRunSkills: (id) => invoke<RunExtensions>({ operation: 'refreshRunSkills', id }),
  approveExtension: (id, approvalId, allow) =>
    invoke<RunExtensions>({ operation: 'approveExtension', id, approvalId, allow }),
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
  chooseWorkspace: () => ipcRenderer.invoke(channels.chooseWorkspace) as Promise<string | null>,
  chooseFiles: () => ipcRenderer.invoke(channels.chooseFiles) as Promise<FileImportResult>,
  importFiles: (files) => {
    if (!Array.isArray(files) || files.length > MAX_REFERENCE_FILES)
      return Promise.reject(new Error('每次最多添加 8 个文件。'))
    return ipcRenderer.invoke(
      channels.importFiles,
      files.map((file) => webUtils.getPathForFile(file)),
    ) as Promise<FileImportResult>
  },
  pasteFiles: () => ipcRenderer.invoke(channels.pasteFiles) as Promise<FileImportResult>,
  getWorkspace: (id) => invoke<WorkspaceReview>({ operation: 'getWorkspace', id }),
  approveCommand: (id, approvalId, allow) =>
    invoke<WorkspaceReview>({ operation: 'approveCommand', id, approvalId, allow }),
  revertFile: (id, path) => invoke<WorkspaceReview>({ operation: 'revertFile', id, path }),
  resumeRun: (id, message, files) =>
    invoke<RunDetail>({ operation: 'resumeRun', id, message, files }),
  cancelRun: (id) => invoke<RunDetail>({ operation: 'cancelRun', id }),
  compactRun: (id) => invoke<RunDetail>({ operation: 'compactRun', id }),
  cancelCompaction: (id) => invoke<RunDetail>({ operation: 'cancelCompaction', id }),
  configureModel: (input) => invoke<ConfigureModelResult>({ operation: 'configureModel', input }),
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
