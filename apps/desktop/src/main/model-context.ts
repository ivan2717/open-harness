import type { ConfigureModelResult, ModelInfo, RunSummary } from '../shared/contracts'
import { resolveModelLimits } from '../shared/model-limits'
import type { Backend } from './backend'
import { fetchModelCapacity } from './model-metadata'
import type { ModelVault } from './model-vault'
import { identifier, resolvedModelConfiguration } from './protocol'

const refreshing = new Set<string>()

/** Refresh an existing connection without returning its credentials to the renderer. */
export async function refreshModelContext(
  id: string,
  backend: Pick<Backend, 'request'>,
  vault: ModelVault,
  discover: typeof fetchModelCapacity = fetchModelCapacity,
): Promise<ConfigureModelResult> {
  identifier(id)
  if (refreshing.has(id)) throw new Error('此模型的上下文容量正在更新。')
  refreshing.add(id)
  try {
    const saved = vault.get(id)
    if (!saved) throw new Error('此模型的连接配置不可用，请重新接入模型。')
    const runs = (await backend.request({ path: '/api/runs', method: 'GET' })) as RunSummary[]
    if (runs.some((run) => run.model_id === id && (run.status === 'running' || run.compacting)))
      throw new Error('此模型仍有运行或压缩任务，请等待结束后更新容量。')

    // Only the window is refreshed. Preserve output and reasoning settings so
    // existing provider replay state remains compatible (notably Messages thinking budgets).
    const input = { ...saved, context_window: undefined }
    const capacity = await discover(input)
    const limits = resolveModelLimits(input, capacity)
    const resolved = resolvedModelConfiguration({ ...input, ...limits })
    const model = (await backend.request({
      path: '/api/models',
      method: 'POST',
      body: resolved,
    })) as ModelInfo
    // Keep both profiles durable until every run has been migrated. If migration
    // fails partway, restart can still restore either binding without losing keys.
    if (!(await vault.save(model.id, resolved)))
      throw new Error('无法保存新的模型容量，现有会话尚未迁移。请检查系统密钥存储后重试。')
    await backend.request({
      path: `/api/models/${id}/replace`,
      method: 'POST',
      body: { replacement_model_id: model.id },
    })
    const cleanedUp = model.id === id || (await vault.remove(id))
    return {
      model,
      persisted: true,
      limits,
      capacity,
      ...(!cleanedUp
        ? { warning: '新容量已保存，但旧模型配置暂未清理；重启后可能仍显示旧配置。' }
        : {}),
    }
  } finally {
    refreshing.delete(id)
  }
}
