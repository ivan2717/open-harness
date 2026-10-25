import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Check, LoaderCircle, Monitor, RefreshCw } from 'lucide-react'
import type { ModelInfo } from '../../shared/contracts'
import { compactNumber, errorMessage } from '../lib/format'
import { configOptions, runsOptions } from '../lib/queries'
import { taskModels } from '../lib/task-models'
import { useUiStore } from '../state/ui-store'
import { InlineError } from './Feedback'

export function ConfiguredModels({
  models,
  selectedId,
}: {
  models: ModelInfo[]
  selectedId?: string
}) {
  const client = useQueryClient()
  const select = useUiStore((state) => state.setSelectedModel)
  const refresh = useMutation({
    mutationFn: (id: string) => window.harness.refreshModelContext(id),
    onSuccess: (result, previousId) => {
      const config = client.getQueryData(configOptions.queryKey)
      const state = useUiStore.getState()
      // Respect a model selection made while the update was in flight.
      if (
        state.selectedModel === previousId ||
        (config && taskModels(config, state.selectedModel).selected?.id === previousId)
      )
        state.setSelectedModel(result.model.id)
    },
    onSettled: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: configOptions.queryKey }),
        client.invalidateQueries({ queryKey: runsOptions.queryKey }),
        client.invalidateQueries({ queryKey: ['run'] }),
      ])
    },
  })

  return (
    <section className="settings-section configured-models">
      <h2>默认模型</h2>
      <p>新会话将使用选中的可用模型。</p>
      <div className="model-cards">
        {models.map((model) => {
          const updating = refresh.isPending && refresh.variables === model.id
          return (
            <div key={model.id} className="settings-model-row">
              <button
                type="button"
                aria-pressed={model.id === selectedId}
                className={`settings-model ${model.id === selectedId ? 'is-selected' : ''}`}
                onClick={() => select(model.id)}
              >
                <span className="settings-model-icon">
                  <Monitor size={20} />
                </span>
                <span className="min-w-0 flex-1">
                  <strong>{model.name}</strong>
                  <p>{model.description}</p>
                  <small>{compactNumber(model.context_window)} 上下文窗口 · 可用</small>
                </span>
                <span className={`selection-ring ${model.id === selectedId ? 'is-selected' : ''}`}>
                  {model.id === selectedId && <Check size={11} />}
                </span>
              </button>
              <button
                type="button"
                className="settings-model-refresh"
                aria-label={`更新 ${model.name} 的上下文容量`}
                disabled={refresh.isPending}
                onClick={() => refresh.mutate(model.id)}
              >
                {updating ? (
                  <LoaderCircle size={14} className="animate-spin" />
                ) : (
                  <RefreshCw size={14} />
                )}
                {updating ? '正在更新…' : '更新上下文容量'}
              </button>
            </div>
          )
        })}
      </div>
      <p className="settings-footnote">
        更新容量无需重填密钥，并应用至使用该模型的空闲会话；运行中需先停止。
      </p>
      {refresh.isError && <InlineError message={errorMessage(refresh.error)} />}
      {refresh.isSuccess && (
        <p role="status" className="settings-footnote">
          {refresh.data.model.name} 的上下文窗口已更新为{' '}
          {compactNumber(refresh.data.model.context_window)}。{refresh.data.warning}
        </p>
      )}
    </section>
  )
}
