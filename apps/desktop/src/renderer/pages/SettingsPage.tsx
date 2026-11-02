import { useQuery } from '@tanstack/react-query'
import { Database, SlidersHorizontal } from 'lucide-react'
import { ErrorState, Loading } from '../components/Feedback'
import { configOptions } from '../lib/queries'
import { useUiStore } from '../state/ui-store'
import { PermissionSettings } from '../components/PermissionSettings'
import { DeveloperTools } from '../components/DeveloperTools'

export function SettingsPage() {
  const config = useQuery(configOptions)
  const inspectorOpen = useUiStore((state) => state.inspectorOpen)
  const setInspector = useUiStore((state) => state.setInspectorOpen)
  if (config.isPending) return <Loading />
  if (config.isError) return <ErrorState error={config.error} retry={() => void config.refetch()} />
  return (
    <div className="settings-page management-page">
      <header className="workspace-header">
        <div className="workspace-heading">
          <SlidersHorizontal size={16} />
          <h1 className="management-page-title">设置</h1>
        </div>
      </header>
      <div className="settings-scroll">
        <div className="settings-content management-content">
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
          <PermissionSettings />
          {Boolean(config.data.recovery_issues?.length) && (
            <section className="settings-section recovery-issues">
              <h2>部分会话未恢复</h2>
              <p>
                原始记录已保留，其他会话可以正常使用。请保留这些文件，修复后重新启动应用可再次恢复。
              </p>
              {config.data.recovery_issues?.map((issue, index) => (
                <details key={`${issue.source_path}-${index}`}>
                  <summary>{issue.message}</summary>
                  <dl>
                    {issue.run_id && (
                      <>
                        <dt>会话</dt>
                        <dd>{issue.run_id}</dd>
                      </>
                    )}
                    <dt>记录位置</dt>
                    <dd>
                      <code>{issue.source_path}</code>
                    </dd>
                    {issue.related_run_id && (
                      <>
                        <dt>关联任务</dt>
                        <dd>{issue.related_run_id}</dd>
                      </>
                    )}
                  </dl>
                </details>
              ))}
            </section>
          )}
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
