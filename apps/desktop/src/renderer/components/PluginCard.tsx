import { useState, type ReactNode } from 'react'
import { useMutation } from '@tanstack/react-query'
import { ChevronDown, Globe, Monitor, LoaderCircle, Puzzle, RefreshCw, Trash2 } from 'lucide-react'
import type {
  BundledPlugin,
  McpServer,
  PluginInfo,
  PluginSourceKind,
  SkillInfo,
} from '../../shared/extensions'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import { SkillList } from './SkillList'
import { PluginImportMenu } from './PluginImportMenu'

export function PluginCard({
  plugin,
  bundled,
  skills,
  busy,
  onToggle,
  onUpdate,
  onUninstall,
  onSkillToggle,
  onInstall,
  onBundledUpdate,
  children,
}: {
  plugin?: PluginInfo
  bundled?: BundledPlugin
  skills: SkillInfo[]
  busy: boolean
  onToggle: () => void
  onUpdate: (sourceKind: PluginSourceKind) => void
  onUninstall: () => void
  onSkillToggle: (id: string, enabled: boolean) => void
  onInstall: () => void
  onBundledUpdate: () => void
  children?: ReactNode
}) {
  const [confirmDelete, setConfirmDelete] = useState(false)
  const name = bundled?.name ?? plugin!.name
  const description = bundled?.description ?? plugin?.description
  return (
    <article className="plugin-card" aria-label={`插件 ${name}`}>
      <div className="extension-row">
        {bundled?.id === 'computer-use' ? (
          <Monitor size={17} className="extension-row-icon" />
        ) : bundled ? (
          <Globe size={17} className="extension-row-icon" />
        ) : (
          <Puzzle size={17} className="extension-row-icon" />
        )}
        <div className="extension-identity">
          <strong>{name}</strong>
          <span>
            {bundled && '内置 · '}
            {plugin ? (
              <>
                {plugin.version} · {plugin.enabled ? '已启用' : '已关闭'}
              </>
            ) : busy ? (
              '正在读取状态…'
            ) : (
              '未安装'
            )}
          </span>
        </div>
        {plugin ? (
          <div className="extension-actions">
            {bundled ? (
              <button
                type="button"
                className="extension-text-button"
                disabled={busy || !bundled.available}
                onClick={onBundledUpdate}
              >
                <RefreshCw size={14} /> 更新内置版本
              </button>
            ) : (
              <PluginImportMenu name={name} disabled={busy} onChoose={onUpdate} />
            )}
            <button
              type="button"
              className="extension-icon-button"
              title="卸载插件"
              aria-label={`卸载插件 ${name}`}
              disabled={busy}
              onClick={() => setConfirmDelete(!confirmDelete)}
            >
              <Trash2 size={14} />
            </button>
            <button
              type="button"
              role="switch"
              aria-checked={plugin.enabled}
              aria-label={`启用插件 ${name}`}
              className={`toggle-switch ${plugin.enabled ? 'is-on' : ''}`}
              disabled={busy}
              onClick={onToggle}
            >
              <span />
            </button>
          </div>
        ) : (
          bundled &&
          bundled.id !== 'chrome-use' && (
            <button
              type="button"
              className="button button-secondary"
              disabled={busy || !bundled.available}
              onClick={onInstall}
            >
              安装
            </button>
          )
        )}
      </div>
      {description && <p className="plugin-description">{description}</p>}
      {bundled?.reason && <p className="plugin-description">{bundled.reason}</p>}
      {children}
      {plugin && (
        <details className="plugin-details">
          <summary>
            <ChevronDown size={13} />
            {plugin.skill_count} 个技能 · {plugin.mcp_servers.length} 个工具服务
          </summary>
          {skills.length > 0 && <SkillList skills={skills} busy={busy} onToggle={onSkillToggle} />}
          {!plugin.enabled && plugin.skill_count > 0 && (
            <p className="extension-hint">启用插件后可查看和使用其中的技能。</p>
          )}
          {plugin.mcp_servers.map((server) => (
            <PluginServer
              key={`${server.id}:${plugin.content_hash}:${plugin.enabled}`}
              server={server}
              enabled={plugin.enabled}
              busy={busy}
              hostConfigured={!!bundled}
            />
          ))}
          <dl className="plugin-metadata">
            <dt>标识</dt>
            <dd>{plugin.id}</dd>
            <dt>安装来源</dt>
            <dd>{plugin.source_path}</dd>
            <dt>内容校验</dt>
            <dd title={plugin.content_hash}>{plugin.content_hash.slice(0, 16)}</dd>
          </dl>
        </details>
      )}
      {plugin && plugin.warnings.length > 0 && (
        <ul className="plugin-warnings">
          {plugin.warnings.map((warning, index) => (
            <li key={index}>{warning}</li>
          ))}
        </ul>
      )}
      {plugin && confirmDelete && (
        <div className="extension-delete-confirm">
          <span>卸载 {name} 及其技能和工具配置？</span>
          <button
            type="button"
            className="extension-text-button"
            disabled={busy}
            onClick={() => {
              setConfirmDelete(false)
              onUninstall()
            }}
          >
            确认卸载
          </button>
          <button
            type="button"
            className="extension-text-button"
            disabled={busy}
            onClick={() => setConfirmDelete(false)}
          >
            取消
          </button>
        </div>
      )}
    </article>
  )
}

function PluginServer({
  server,
  enabled,
  busy,
  hostConfigured,
}: {
  server: McpServer
  enabled: boolean
  busy: boolean
  hostConfigured: boolean
}) {
  const test = useMutation({ mutationFn: () => window.harness.testMcp(server.id) })
  const required =
    server.transport.type === 'stdio'
      ? server.transport.env_vars
      : server.transport.bearer_token_env
        ? [server.transport.bearer_token_env]
        : []
  return (
    <div className="plugin-server">
      <div className="extension-row">
        <div className="extension-identity">
          <strong>{server.name || server.id}</strong>
          <span>
            {server.transport.type === 'stdio' ? '本地命令' : '远程服务'} ·{' '}
            {test.isSuccess
              ? `${hostConfigured ? '工具服务可用' : '已连接'} · ${test.data.tools.length} 个工具`
              : '尚未测试连接'}
          </span>
        </div>
        <button
          type="button"
          className="extension-text-button"
          disabled={busy || !enabled || !server.enabled || test.isPending}
          title="连接服务并读取工具列表"
          onClick={() => test.mutate()}
        >
          {test.isPending ? <LoaderCircle size={14} className="animate-spin" /> : null}
          {test.isPending ? '连接中' : '测试连接'}
        </button>
      </div>
      {!hostConfigured && required.length > 0 && (
        <p className="extension-hint">运行前需设置环境变量：{required.join('、')}</p>
      )}
      {test.isError && <InlineError message={errorMessage(test.error)} />}
    </div>
  )
}
