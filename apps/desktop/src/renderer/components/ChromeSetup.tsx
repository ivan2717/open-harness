import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import type { BundledPlugin, PluginInfo } from '../../shared/extensions'
import { InlineError } from './Feedback'
import { errorMessage } from '../lib/format'
import { extensionSettingsOptions } from '../lib/extensions'

export function ChromeSetup({
  connection,
  disabled = false,
  installed,
}: {
  connection: NonNullable<BundledPlugin['chrome']>
  disabled?: boolean
  installed?: PluginInfo
}) {
  const client = useQueryClient()
  const [watching, setWatching] = useState(false)
  const [timedOut, setTimedOut] = useState(false)
  const status = useQuery({
    queryKey: ['chrome-connection'],
    queryFn: () => window.harness.chromeConnectionStatus(),
    initialData: connection,
    initialDataUpdatedAt: 0,
    staleTime: 5_000,
    retry: false,
    refetchOnWindowFocus: 'always',
    // Only poll Chrome during setup, not the native desktop permission probe.
    refetchInterval: (query) => (watching && !query.state.data?.connected ? 2_000 : false),
  })
  const { refetch } = status
  useEffect(() => {
    // The bundled catalog also probes desktop permissions and can return late.
    // Only the dedicated Chrome query should update live connection status.
    const refresh = () => void refetch()
    window.addEventListener('focus', refresh)
    return () => window.removeEventListener('focus', refresh)
  }, [refetch])
  useEffect(() => {
    if (!watching) return
    const timer = setTimeout(() => {
      setWatching(false)
      setTimedOut(true)
    }, 120_000)
    return () => clearTimeout(timer)
  }, [watching])
  useEffect(() => {
    if (status.data.connected) {
      setWatching(false)
      setTimedOut(false)
    }
  }, [status.data.connected, watching])
  const connect = useMutation({
    scope: { id: 'extension-config' },
    mutationFn: (mode: 'store' | 'development' | 'repair') => window.harness.connectChrome(mode),
    onMutate: () => {
      setWatching(false)
      setTimedOut(false)
    },
    onSuccess: () => setWatching(true),
    onSettled: () => {
      void client.invalidateQueries({ queryKey: ['chrome-connection'] })
      void client.invalidateQueries({ queryKey: extensionSettingsOptions.queryKey })
      void client.invalidateQueries({ queryKey: ['skill-catalog'] })
      void client.invalidateQueries({ queryKey: ['skill-picker'] })
    },
  })
  const copy = useMutation({
    mutationFn: () => window.harness.copyText(status.data.extensionPath),
  })
  const current = status.data
  const busy = disabled || connect.isPending
  const platform = typeof window === 'undefined' ? undefined : window.harness.platform
  return (
    <div className="plugin-description">
      <div className="approval-actions">
        <span role="status">
          {installed && !installed.enabled
            ? current.connected
              ? '插件已关闭 · Chrome 连接已保留'
              : '插件已关闭 · Chrome 未连接'
            : current.connected
              ? 'Chrome 已连接'
              : watching
                ? '等待 Chrome 连接…'
                : 'Chrome 未连接'}
        </span>
        {!current.connected && (
          <button
            type="button"
            className="button button-secondary"
            disabled={busy || !current.storeUrl}
            onClick={() => connect.mutate('store')}
          >
            {connect.isPending ? '正在准备…' : '安装 Chrome 扩展'}
          </button>
        )}
        {current.connected && !installed && (
          <button
            type="button"
            className="button button-secondary"
            disabled={busy}
            onClick={() => connect.mutate('repair')}
          >
            启用 Chrome
          </button>
        )}
        <button
          type="button"
          className="extension-text-button"
          disabled={status.isFetching || connect.isPending}
          onClick={() => void status.refetch()}
        >
          检查连接
        </button>
      </div>
      {!current.connected && !current.storeUrl && (
        <p>Chrome 扩展尚未上架，暂不能从商店安装。上架后可在这里直接安装，无需开发者模式。</p>
      )}
      {!current.connected && current.storeUrl && (
        <p>点击后在 Chrome 商店确认「添加扩展程序」，这里会自动检测连接。</p>
      )}
      {timedOut && !current.connected && (
        <p role="status">尚未检测到连接。完成 Chrome 中的安装确认后，可以点击「检查连接」。</p>
      )}
      {current.connected && installed?.enabled && !current.allowNewTabs && (
        <p>在 Chrome 工具栏的 Open Harness 扩展中允许创建后台标签页，或仅共享当前网页。</p>
      )}
      <details>
        <summary>连接帮助</summary>
        <p>
          已安装扩展却未连接时，可启用插件并重新准备连接，然后在 Chrome
          中重新加载扩展。已有网页仍需从扩展中共享。
        </p>
        <button
          type="button"
          className="extension-text-button"
          disabled={busy}
          onClick={() => connect.mutate('repair')}
        >
          启用并修复连接
        </button>
      </details>
      <details>
        <summary>开发者选项</summary>
        <p>
          先点击「打开本地扩展目录」，准备扩展并启用插件。然后在 Chrome
          扩展页开启「开发者模式」，点击「加载已解压的扩展程序」，选择这个文件夹。
        </p>
        <button
          type="button"
          className="extension-text-button"
          disabled={busy}
          onClick={() => connect.mutate('development')}
        >
          打开本地扩展目录
        </button>
        <button
          type="button"
          className="extension-text-button"
          disabled={busy || copy.isPending}
          onClick={() => copy.mutate()}
        >
          {copy.isSuccess ? '已复制目录' : '复制扩展目录'}
        </button>
        <p>
          {platform === 'win32'
            ? '如果选择窗口没有进入该目录，按 Ctrl+L，在地址栏粘贴下面的路径后按回车，再点击「选择文件夹」。'
            : platform === 'darwin'
              ? '如果选择窗口没有进入该目录，按 ⌘⇧G，粘贴下面的路径后按回车，再点击「选择」。'
              : '如果选择窗口没有进入该目录，在位置栏粘贴下面的路径，再选择这个文件夹。'}
        </p>
        <code>{current.extensionPath}</code>
      </details>
      {current.installationError && <InlineError message={current.installationError} />}
      {current.error && <InlineError message={current.error} />}
      {(connect.error || status.error || copy.error) && (
        <InlineError message={errorMessage(connect.error || status.error || copy.error)} />
      )}
    </div>
  )
}
