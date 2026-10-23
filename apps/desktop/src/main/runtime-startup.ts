import type { ResolvedModelConfiguration } from '../shared/contracts'
import type { Backend } from './backend'

/** Keychain decryption may block the main thread. Start the runtime only after it finishes. */
export async function initializeRuntime(
  backend: Pick<Backend, 'request' | 'start'>,
  loadModels: () => Promise<ResolvedModelConfiguration[]>,
  reportModelError: (message: string) => void,
): Promise<void> {
  let models: ResolvedModelConfiguration[] = []
  try {
    models = await loadModels()
  } catch {
    reportModelError('已保存的模型配置未能恢复。可在这里重新接入；本地会话记录仍然可用。')
  }
  for (const input of models) {
    try {
      // The first request starts the runtime, after any keychain confirmation.
      await backend.request({ path: '/api/models', method: 'POST', body: input })
    } catch {
      reportModelError('部分模型配置未能恢复，可重新接入对应模型。')
    }
  }
  // Also start when the vault is empty or cannot be decrypted.
  await backend.start()
}

export async function watchInitializedRuntime(
  initialization: Promise<void>,
  signal: AbortSignal,
  watch: () => Promise<void>,
): Promise<void> {
  await initialization
  if (!signal.aborted) await watch()
}
