import type { ModelRestoreState, ResolvedModelConfiguration } from '../shared/contracts'
import type { Backend } from './backend'

/** Restore credentials independently of the runtime/history connection. */
export class ModelRestoration {
  private current: ModelRestoreState = { status: 'loading' }
  private pending: Promise<void> | undefined

  constructor(
    private backend: Pick<Backend, 'request'>,
    private loadModels: () => Promise<ResolvedModelConfiguration[]>,
  ) {}

  snapshot(): ModelRestoreState {
    return { ...this.current }
  }

  restore(): Promise<void> {
    if (this.pending) return this.pending
    this.current = { status: 'loading' }
    this.pending = this.loadAndRegister().finally(() => {
      this.pending = undefined
    })
    return this.pending
  }

  private async loadAndRegister(): Promise<void> {
    let models: ResolvedModelConfiguration[]
    try {
      models = await this.loadModels()
    } catch {
      this.current = {
        status: 'error',
        message:
          '未能解锁已保存的模型配置。本地历史仍可查看；可以重试解锁，或重新接入模型供本次使用。',
      }
      return
    }
    let failed = false
    for (const input of models) {
      try {
        await this.backend.request({ path: '/api/models', method: 'POST', body: input })
      } catch {
        failed = true
      }
    }
    this.current = failed
      ? { status: 'error', message: '部分模型未能恢复到本地服务。可以重试恢复；不会自动继续任务。' }
      : { status: 'ready' }
  }
}
