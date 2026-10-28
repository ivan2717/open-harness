import type {
  AppConfig,
  ConfigureModelResult,
  ModelConfiguration,
  ModelInfo,
  RunSummary,
  UpdateModelInput,
} from '../shared/contracts'
import { resolveModelLimits } from '../shared/model-limits'
import { resolveReasoningCapability } from '../shared/model-reasoning'
import type { Backend } from './backend'
import { withModelConfigurationUpdate } from './model-context'
import { fetchModelMetadata } from './model-metadata'
import type { ModelVault } from './model-vault'
import {
  identifier,
  modelConfiguration,
  modelUpdateConfiguration,
  resolvedModelConfiguration,
} from './protocol'

export function getModelConfiguration(id: string, vault: ModelVault): ModelConfiguration {
  const saved = vault.get(identifier(id))
  if (!saved) throw new Error('此模型的连接配置不可用，请重新接入模型。')
  const { api_key, ...configuration } = saved
  return { ...configuration, id, has_api_key: Boolean(api_key) }
}

/** Update a saved connection while credentials remain confined to the main process. */
export async function updateModelConfiguration(
  id: string,
  input: UpdateModelInput,
  backend: Pick<Backend, 'request'>,
  vault: ModelVault,
  discover: typeof fetchModelMetadata = fetchModelMetadata,
): Promise<ConfigureModelResult> {
  return withModelConfigurationUpdate(id, async () => {
    const saved = vault.get(id)
    if (!saved) throw new Error('此模型的连接配置不可用，请重新接入模型。')
    const update = modelUpdateConfiguration(input)
    if (!update.api_key && new URL(update.base_url).origin !== new URL(saved.base_url).origin)
      throw new Error('API 服务地址已改变，请填写该服务的 API key；已有密钥不会发送给其他服务。')
    const runs = (await backend.request({ path: '/api/runs', method: 'GET' })) as RunSummary[]
    if (runs.some((run) => run.model_id === id && (run.status === 'running' || run.compacting)))
      throw new Error('此模型仍有运行或压缩任务，请等待结束后编辑。')
    const configuration = modelConfiguration({
      ...update,
      api_key: update.api_key || saved.api_key,
    })
    const metadata = await discover(configuration)
    const capacity = metadata?.capacity
    const limits = resolveModelLimits(configuration, capacity)
    const resolved = resolvedModelConfiguration({
      ...configuration,
      context_window: limits.context_window,
      max_output_tokens: limits.max_output_tokens,
      ...(configuration.reasoning_mode === 'off'
        ? {
            reasoning_capability:
              configuration.reasoning_capability ??
              resolveReasoningCapability(configuration, metadata?.reasoning),
          }
        : {}),
    })
    const migrate = async (model: ModelInfo): Promise<ConfigureModelResult> => {
      // Keep both profiles until the backend has durably updated their run bindings.
      await backend.request({
        path: `/api/models/${id}/replace`,
        method: 'POST',
        body: { replacement_model_id: model.id, allow_configuration_change: true },
      })
      const cleanedUp = model.id === id || (await vault.remove(id))
      return {
        model,
        persisted: true,
        limits,
        capacity,
        ...(!cleanedUp
          ? { warning: '新配置已保存，但旧模型配置暂未清理；重启后可能仍显示旧配置。' }
          : {}),
      }
    }
    const register = async (): Promise<ConfigureModelResult> => {
      const model = (await backend.request({
        path: '/api/models',
        method: 'POST',
        body: { ...resolved, edit_source_id: id },
      })) as ModelInfo
      if (!(await vault.save(model.id, resolved, { requirePersistence: true }))) {
        // Secret rotation and vision changes can preserve the deterministic ID.
        // Restore its old runtime registration if the durable replacement failed.
        if (model.id === id) {
          try {
            await backend.request({
              path: '/api/models',
              method: 'POST',
              body: { ...saved, edit_source_id: id },
            })
          } catch {
            throw new Error(
              '无法保存模型配置，也未能恢复当前连接。旧配置仍已保存，请重新启动应用。',
            )
          }
        }
        throw new Error('无法保存模型配置，现有会话尚未迁移。请检查系统密钥存储后重试。')
      }
      return migrate(model)
    }
    const replacementId = vault.findEquivalent(resolved, id)
    if (replacementId) {
      // A previous attempt may have saved its replacement but failed migration.
      // Reuse that durable registration across restarts, without overwriting it.
      return withModelConfigurationUpdate(replacementId, async () => {
        const config = (await backend.request({ path: '/api/config', method: 'GET' })) as AppConfig
        const existing = config.models.find(
          (model) => model.id === replacementId && model.available,
        )
        return existing ? migrate(existing) : register()
      })
    }
    return register()
  })
}
