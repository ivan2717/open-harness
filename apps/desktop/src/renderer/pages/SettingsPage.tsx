import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Check, Database, Monitor, SlidersHorizontal } from 'lucide-react'
import { ErrorState, Loading } from '../components/Feedback'
import { compactNumber } from '../lib/format'
import { configOptions } from '../lib/queries'
import { useUiStore } from '../state/ui-store'
import { ModelSettings } from '../components/ModelSettings'
import { DeveloperTools } from '../components/DeveloperTools'
import { ExtensionsSettings } from '../components/ExtensionsSettings'
import { taskModels } from '../lib/task-models'

export function SettingsPage() {
  const config = useQuery(configOptions)
  const selected = useUiStore((state) => state.selectedModel)
  const select = useUiStore((state) => state.setSelectedModel)
  const inspectorOpen = useUiStore((state) => state.inspectorOpen)
  const setInspector = useUiStore((state) => state.setInspectorOpen)
  if (config.isPending) return <Loading />
  if (config.isError) return <ErrorState error={config.error} retry={() => void config.refetch()} />
  const { models, selected: activeModel } = taskModels(config.data, selected)
  return (
    <div className="settings-page">
      <header className="workspace-header">
        <div className="workspace-heading">
          <SlidersHorizontal size={16} />
          工作区设置
        </div>
        <Link to="/" className="inspector-toggle">
          返回任务
        </Link>
      </header>
      <div className="settings-scroll">
        <div className="settings-content">
          <div className="settings-intro">
            <span className="welcome-eyebrow">MAKE IT YOURS</span>
            <h1>你的工作区。</h1>
            <p>模型接入由本地服务管理，界面偏好保存在这台设备上。</p>
          </div>
          {models.length > 0 && (
            <section className="settings-section">
              <h2>默认模型</h2>
              <p>新会话将使用选中的可用模型。</p>
              <div className="model-cards">
                {models.map((model) => (
                  <button
                    key={model.id}
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
          <ExtensionsSettings />
          <section className="settings-section">
            <h2>界面偏好</h2>
            <div className="settings-row">
              <div>
                <strong>显示运行检查器</strong>
                <p>在会话右侧展示上下文预算、检查点与运行时间线。</p>
              </div>
              <button
                role="switch"
                aria-checked={inspectorOpen}
                aria-label="显示运行检查器"
                className={`toggle-switch ${inspectorOpen ? 'is-on' : ''}`}
                onClick={() => setInspector(!inspectorOpen)}
              >
                <span />
              </button>
            </div>
          </section>
          <section className="settings-section">
            <h2>本地数据</h2>
            <div className="storage-card">
              <Database size={19} />
              <div>
                <strong>运行记录目录</strong>
                <code>{config.data.storage_path}</code>
              </div>
            </div>
            <p className="settings-footnote">
              会话与运行事件由本地服务持久化。草稿和界面偏好保存在桌面应用中。
            </p>
          </section>
          {config.data.models.some((model) => model.id === 'local-demo' && model.available) && (
            <DeveloperTools />
          )}
          <div className="settings-about">
            <span>Open Harness</span>
            <span>
              v{config.data.version} · {window.harness.platform}
            </span>
          </div>
        </div>
      </div>
    </div>
  )
}
