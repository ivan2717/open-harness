import { useQuery } from '@tanstack/react-query'
import { Cpu } from 'lucide-react'
import { ConfiguredModels } from '../components/ConfiguredModels'
import { ErrorState, Loading } from '../components/Feedback'
import { ModelSettings } from '../components/ModelSettings'
import { configOptions } from '../lib/queries'
import { taskModels } from '../lib/task-models'
import { useUiStore } from '../state/ui-store'

export function ModelsPage() {
  const config = useQuery(configOptions)
  const selected = useUiStore((state) => state.selectedModel)
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
          {models.length > 0 && <ConfiguredModels models={models} selectedId={activeModel?.id} />}
          {config.data.model_config_error && <p role="alert">{config.data.model_config_error}</p>}
          <ModelSettings />
        </div>
      </div>
    </div>
  )
}
