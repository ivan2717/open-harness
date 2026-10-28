import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Cpu } from 'lucide-react'
import { ConfiguredModels } from '../components/ConfiguredModels'
import { ErrorState, InlineError, Loading } from '../components/Feedback'
import { ModelSettings } from '../components/ModelSettings'
import { configOptions } from '../lib/queries'
import { taskModels } from '../lib/task-models'
import { useUiStore } from '../state/ui-store'
import { errorMessage } from '../lib/format'
import type { ConfigureModelResult } from '../../shared/contracts'

export function ModelsPage() {
  const config = useQuery(configOptions)
  const selected = useUiStore((state) => state.selectedModel)
  const [editingId, setEditingId] = useState<string>()
  const [savedNotice, setSavedNotice] = useState('')
  const editor = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (editingId) editor.current?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }, [editingId])
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
            <ConfiguredModels
              models={models}
              selectedId={activeModel?.id}
              editingId={editingId}
              onEdit={(id) => {
                setSavedNotice('')
                setEditingId(id)
              }}
            />
          )}
          {config.data.model_config_error && <p role="alert">{config.data.model_config_error}</p>}
          {savedNotice && (
            <p role="status" className="settings-footnote">
              {savedNotice}
            </p>
          )}
          <div ref={editor} className="model-editor">
            {editingId ? (
              <SavedModelEditor
                key={editingId}
                id={editingId}
                onCancel={() => setEditingId(undefined)}
                onSaved={(result) => {
                  setEditingId(undefined)
                  setSavedNotice(`模型配置已更新。${result.warning ?? ''}`)
                }}
              />
            ) : (
              <ModelSettings />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

function SavedModelEditor({
  id,
  onSaved,
  onCancel,
}: {
  id: string
  onSaved: (result: ConfigureModelResult) => void
  onCancel: () => void
}) {
  const configuration = useQuery({
    queryKey: ['model-configuration', id],
    queryFn: () => window.harness.getModelConfiguration(id),
    staleTime: 0,
    gcTime: 0,
    retry: false,
  })
  if (configuration.isPending) return <Loading label="正在读取模型配置" />
  if (configuration.isError)
    return (
      <section className="settings-section">
        <h2>编辑模型</h2>
        <InlineError
          message={errorMessage(configuration.error)}
          retry={() => void configuration.refetch()}
        />
        <button type="button" className="model-cancel" onClick={onCancel}>
          取消编辑
        </button>
      </section>
    )
  return <ModelSettings initial={configuration.data} onSaved={onSaved} onCancel={onCancel} />
}
