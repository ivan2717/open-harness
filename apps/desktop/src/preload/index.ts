import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron'
import type {
  AppConfig,
  ConfigureModelResult,
  DesktopBridge,
  RunDetail,
  RunSummary,
  WorkspaceReview,
} from '../shared/contracts'
import { channels, type BridgeRequest } from '../main/protocol'

const invoke = <T>(request: BridgeRequest): Promise<T> =>
  ipcRenderer.invoke(channels.invoke, request) as Promise<T>
let nextSubscription = 0

const bridge: DesktopBridge = {
  config: () => invoke<AppConfig>({ operation: 'config' }),
  listRuns: () => invoke<RunSummary[]>({ operation: 'listRuns' }),
  getRun: (id) => invoke<RunDetail>({ operation: 'getRun', id }),
  createRun: (input) => invoke<RunDetail>({ operation: 'createRun', input }),
  chooseWorkspace: () => ipcRenderer.invoke(channels.chooseWorkspace) as Promise<string | null>,
  getWorkspace: (id) => invoke<WorkspaceReview>({ operation: 'getWorkspace', id }),
  approveCommand: (id, approvalId, allow) =>
    invoke<WorkspaceReview>({ operation: 'approveCommand', id, approvalId, allow }),
  revertFile: (id, path) => invoke<WorkspaceReview>({ operation: 'revertFile', id, path }),
  resumeRun: (id, message) => invoke<RunDetail>({ operation: 'resumeRun', id, message }),
  cancelRun: (id) => invoke<RunDetail>({ operation: 'cancelRun', id }),
  compactRun: (id) => invoke<RunDetail>({ operation: 'compactRun', id }),
  cancelCompaction: (id) => invoke<RunDetail>({ operation: 'cancelCompaction', id }),
  configureModel: (input) => invoke<ConfigureModelResult>({ operation: 'configureModel', input }),
  submitInput: (id, message) => invoke<RunDetail>({ operation: 'submitInput', id, message }),
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
