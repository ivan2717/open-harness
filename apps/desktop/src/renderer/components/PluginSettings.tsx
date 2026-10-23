import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import {
  Archive,
  BookOpen,
  ChevronDown,
  FolderOpen,
  LoaderCircle,
  Plus,
  Puzzle,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import type {
  BuiltinPluginInfo,
  ExtensionSettings,
  McpServer,
  PluginInfo,
  PluginSourceKind,
  SkillInfo,
} from '../../shared/extensions'
import {
  pluginErrorMessage,
  usePluginActions,
  useSaveExtensions,
  withSkillEnabled,
} from '../lib/extensions'
import { errorMessage } from '../lib/format'
import { ComposerMenu } from './ComposerMenu'
import { InlineError } from './Feedback'
import { SkillList } from './SkillList'

export function PluginSettings({
  settings,
  loading,
  error,
  retry,
}: {
  settings?: ExtensionSettings
  loading: boolean
  error: unknown
  retry: () => void
}) {
  const action = usePluginActions()
  const save = useSaveExtensions()
  const busy = action.isPending || save.isPending
  return (
    <section className="settings-section extension-settings">
      <div className="extension-section-heading">
        <div>
          <h2>插件</h2>
          <p>一起安装和管理任务所需的技能与工具。</p>
        </div>
        <PluginImportMenu
          disabled={!settings || busy}
          onChoose={(sourceKind) => action.mutate({ type: 'install', sourceKind })}
        />
      </div>
      {loading && <p className="extension-hint">正在读取插件…</p>}
      {error != null && <InlineError message={errorMessage(error)} retry={retry} />}
      <div className="extension-list">
        {(settings?.builtin_plugins ?? []).map((plugin) => (
          <BuiltinPluginCard
            key={plugin.id}
            plugin={plugin}
            busy={busy}
            onToggle={() =>
              action.mutate({ type: 'enabled', id: plugin.id, enabled: !plugin.enabled })
            }
          />
        ))}
        {(settings?.plugins ?? []).map((plugin) => (
          <PluginCard
            key={plugin.id}
            plugin={plugin}
            skills={settings!.skills.filter((skill) => skill.plugin_id === plugin.id)}
            busy={busy}
            onToggle={() =>
              action.mutate({ type: 'enabled', id: plugin.id, enabled: !plugin.enabled })
            }
            onUpdate={(sourceKind) => action.mutate({ type: 'install', id: plugin.id, sourceKind })}
            onUninstall={() => action.mutate({ type: 'uninstall', id: plugin.id })}
            onSkillToggle={(id, enabled) =>
              save.mutate((config) => withSkillEnabled(config, id, enabled))
            }
          />
        ))}
      </div>
      {settings && !(settings.plugins ?? []).length && (
        <p className="extension-empty">也可以从文件夹或 ZIP 安装更多插件。</p>
      )}
      {action.isPending && (
        <p className="extension-hint" role="status">
          <LoaderCircle size={12} className="animate-spin" /> 正在处理插件…
        </p>
      )}
      {action.isError && <InlineError message={pluginErrorMessage(action.error)} />}
      {save.isError && <InlineError message={errorMessage(save.error)} />}
      <p className="settings-footnote">
        安装外部插件不会连接服务或运行脚本。启用后，Agent 可按需发现工具；外部操作仍需单独批准。
      </p>
    </section>
  )
}

export function BuiltinPluginCard({
  plugin,
  busy,
  onToggle,
}: {
  plugin: BuiltinPluginInfo
  busy: boolean
  onToggle: () => void
}) {
  return (
    <article className="plugin-card" aria-label={`插件 ${plugin.name}`}>
      <div className="extension-row">
        <BookOpen size={17} className="extension-row-icon" />
        <div className="extension-identity">
          <strong>{plugin.name}</strong>
          <span>内置 · {plugin.enabled ? '已启用' : '已关闭'}</span>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={plugin.enabled}
          aria-label={`启用插件 ${plugin.name}`}
          className={`toggle-switch ${plugin.enabled ? 'is-on' : ''}`}
          disabled={busy}
          onClick={onToggle}
        >
          <span />
        </button>
      </div>
      <p className="plugin-description">{plugin.description}</p>
      <p className="plugin-description">
        在运行检查器的「记忆」中查看。关闭后停止自动整理与 Wiki
        读取，已保存记录保留。上下文压缩和原始会话回查不受影响。
      </p>
    </article>
  )
}

function PluginImportMenu({
  name,
  disabled,
  onChoose,
}: {
  name?: string
  disabled: boolean
  onChoose: (sourceKind: PluginSourceKind) => void
}) {
  return (
    <ComposerMenu
      label={name ? `更新插件 ${name}` : '安装插件'}
      className="extension-text-button plugin-import-button"
      align="end"
      disabled={disabled}
      trigger={
        <>
          {name ? <RefreshCw size={14} /> : <Plus size={15} />}
          {name ? '更新' : '安装插件'}
          <ChevronDown size={12} />
        </>
      }
    >
      <button
        type="button"
        role="menuitem"
        className="composer-menu-item"
        onClick={() => onChoose('directory')}
      >
        <FolderOpen size={15} />
        从文件夹{name ? '更新' : '安装'}
      </button>
      <button
        type="button"
        role="menuitem"
        className="composer-menu-item"
        onClick={() => onChoose('zip')}
      >
        <Archive size={15} />从 ZIP {name ? '更新' : '安装'}
      </button>
    </ComposerMenu>
  )
}

function PluginCard({
  plugin,
  skills,
  busy,
  onToggle,
  onUpdate,
  onUninstall,
  onSkillToggle,
}: {
  plugin: PluginInfo
  skills: SkillInfo[]
  busy: boolean
  onToggle: () => void
  onUpdate: (sourceKind: PluginSourceKind) => void
  onUninstall: () => void
  onSkillToggle: (id: string, enabled: boolean) => void
}) {
  const [confirmDelete, setConfirmDelete] = useState(false)
  return (
    <article className="plugin-card" aria-label={`插件 ${plugin.name}`}>
      <div className="extension-row">
        <Puzzle size={17} className="extension-row-icon" />
        <div className="extension-identity">
          <strong>{plugin.name}</strong>
          <span>
            {plugin.version} · {plugin.enabled ? '已启用' : '已关闭'}
          </span>
        </div>
        <div className="extension-actions">
          <PluginImportMenu name={plugin.name} disabled={busy} onChoose={onUpdate} />
          <button
            type="button"
            className="extension-icon-button"
            title="卸载插件"
            aria-label={`卸载插件 ${plugin.name}`}
            disabled={busy}
            onClick={() => setConfirmDelete(!confirmDelete)}
          >
            <Trash2 size={14} />
          </button>
          <button
            type="button"
            role="switch"
            aria-checked={plugin.enabled}
            aria-label={`启用插件 ${plugin.name}`}
            className={`toggle-switch ${plugin.enabled ? 'is-on' : ''}`}
            disabled={busy}
            onClick={onToggle}
          >
            <span />
          </button>
        </div>
      </div>
      {plugin.description && <p className="plugin-description">{plugin.description}</p>}
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
      {plugin.warnings.length > 0 && (
        <ul className="plugin-warnings">
          {plugin.warnings.map((warning, index) => (
            <li key={index}>{warning}</li>
          ))}
        </ul>
      )}
      {confirmDelete && (
        <div className="extension-delete-confirm">
          <span>卸载 {plugin.name} 及其技能和工具配置？</span>
          <button
            type="button"
            className="extension-text-button"
            disabled={busy}
            onClick={onUninstall}
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
}: {
  server: McpServer
  enabled: boolean
  busy: boolean
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
            {test.isSuccess ? `已连接 · ${test.data.tools.length} 个工具` : '尚未测试连接'}
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
      {required.length > 0 && (
        <p className="extension-hint">运行前需设置环境变量：{required.join('、')}</p>
      )}
      {test.isError && <InlineError message={errorMessage(test.error)} />}
    </div>
  )
}
