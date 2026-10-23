import { useQuery } from '@tanstack/react-query'
import { Check, Cpu, Monitor } from 'lucide-react'
import { ErrorState, Loading } from '../components/Feedback'
import { ModelSettings } from '../components/ModelSettings'
import { compactNumber } from '../lib/format'
import { configOptions } from '../lib/queries'
import { taskModels } from '../lib/task-models'
import { useUiStore } from '../state/ui-store'

export function ModelsPage() {
  const config = useQuery(configOptions)
  const selected = useUiStore((state) => state.selectedModel)
  const select = useUiStore((state) => state.setSelectedModel)
  if (config.isPending) return <Loading label="正在读取模型配置" />
  if (config.isError) return <ErrorState error={config.error} retry={() => void config.refetch()} />
  const { models, selected: activeModel } = taskModels(config.data, selected)
  return (
    <div className="settings-page management-page">
      <header className="workspace-header">
        <div className="workspace-heading">
          <Cpu size={16} />
          <h1 className="management-page-title">模型</h1>
        </div>
      </header>
      <div className="settings-scroll">
        <div className="settings-content management-content">
          {models.length > 0 && (
            <section className="settings-section">
              <h2>默认模型</h2>
              <p>新会话将使用选中的可用模型。</p>
              <div className="model-cards">
                {models.map((model) => (
                  <button
                    key={model.id}
                    type="button"
                    aria-pressed={model.id === activeModel?.id}
                    className={`settings-model ${model.id === activeModel?.id ? 'is-selected' : ''}`}
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
                    <span
                      className={`selection-ring ${model.id === activeModel?.id ? 'is-selected' : ''}`}
                    >
                      {model.id === activeModel?.id && <Check size={11} />}
                    </span>
                  </button>
                ))}
              </div>
            </section>
          )}
          {config.data.model_config_error && <p role="alert">{config.data.model_config_error}</p>}
          <ModelSettings />
        </div>
      </div>
    </div>
  )
}
