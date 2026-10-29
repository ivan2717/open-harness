import { useIsMutating, useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BookOpen, LoaderCircle } from 'lucide-react'
import type {
  BuiltinPluginInfo,
  BundledPlugin,
  ExtensionSettings,
  PluginInfo,
} from '../../shared/extensions'
import {
  extensionSettingsOptions,
  pluginErrorMessage,
  useBundledPluginCatalog,
  usePluginActions,
  useSaveExtensions,
  withSkillEnabled,
} from '../lib/extensions'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import { PluginImportMenu } from './PluginImportMenu'
import { PluginCard } from './PluginCard'
import { BundledPluginSetup } from './BundledPluginSetup'

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
  const client = useQueryClient()
  const catalog = useBundledPluginCatalog()
  const action = usePluginActions()
  const save = useSaveExtensions()
  const install = useMutation({
    scope: { id: 'extension-config' },
    mutationFn: (id: BundledPlugin['id']) => window.harness.installBundledPlugin(id),
    onSuccess: (data) => {
      client.setQueryData(extensionSettingsOptions.queryKey, data)
      void client.invalidateQueries({ queryKey: ['run-extensions'] })
      void client.invalidateQueries({ queryKey: ['skill-catalog'] })
      void client.invalidateQueries({ queryKey: ['skill-picker'] })
    },
  })
  const busy =
    useIsMutating({ predicate: (mutation) => mutation.options.scope?.id === 'extension-config' }) >
    0
  const bundled = catalog.data ?? []
  function card(plugin: PluginInfo | undefined, bundle?: BundledPlugin) {
    const id = plugin?.id ?? bundle!.id
    const permissions = (settings?.control_permissions ?? []).filter((permission) =>
      plugin?.mcp_servers.some((server) => server.id === permission.server_id),
    )
    const hasControl = ['chrome-use', 'browser-use', 'computer-use'].includes(id)
    return (
      <PluginCard
        key={id}
        plugin={plugin}
        bundled={bundle}
        skills={settings?.skills.filter((skill) => skill.plugin_id === id) ?? []}
        busy={busy || !settings}
        onInstall={() => bundle && install.mutate(bundle.id)}
        onToggle={() => plugin && action.mutate({ type: 'enabled', id, enabled: !plugin.enabled })}
        onUpdate={(sourceKind) => action.mutate({ type: 'install', id, sourceKind })}
        onBundledUpdate={() => bundle && install.mutate(bundle.id)}
        onUninstall={() => action.mutate({ type: 'uninstall', id })}
        onSkillToggle={(skillId, enabled) =>
          save.mutate((config) => withSkillEnabled(config, skillId, enabled))
        }
      >
        {bundle && (
          <BundledPluginSetup plugin={bundle} installed={plugin} disabled={busy || !settings} />
        )}
        {plugin && (hasControl || permissions.length > 0) && (
          <div className="plugin-control-permissions">
            <span>
              {permissions.length ? `已保存 ${permissions.length} 项访问授权` : '尚未保存长期授权'}
            </span>
            <Link to="/settings" className="extension-text-button">
              管理访问授权
            </Link>
          </div>
        )}
      </PluginCard>
    )
  }
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
      {catalog.isPending && (
        <p className="extension-hint" role="status">
          正在检查内置能力…
        </p>
      )}
      {catalog.error && (
        <InlineError message={errorMessage(catalog.error)} retry={() => void catalog.refetch()} />
      )}
      <div className="extension-list">
        {bundled.map((bundle) =>
          card(
            settings?.plugins.find((plugin) => plugin.id === bundle.id),
            bundle,
          ),
        )}
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
        {/* Fall back to installed cards if the bundled catalog is unavailable. */}
        {(settings?.plugins ?? [])
          .filter((plugin) => !bundled.some((bundle) => bundle.id === plugin.id))
          .map((plugin) => card(plugin))}
      </div>
      {settings && !(settings.plugins ?? []).length && (
        <p className="extension-empty">也可以从文件夹或 ZIP 安装更多插件。</p>
      )}
      {busy && (
        <p className="extension-hint" role="status">
          <LoaderCircle size={12} className="animate-spin" /> 正在处理插件…
        </p>
      )}
      {action.isError && <InlineError message={pluginErrorMessage(action.error)} />}
      {install.isError && <InlineError message={pluginErrorMessage(install.error)} />}
      {save.isError && <InlineError message={errorMessage(save.error)} />}
      <p className="settings-footnote">
        安装外部插件不会连接服务或运行脚本。启用后，Agent 可按需发现工具，工具使用遵循你授予的权限。
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
