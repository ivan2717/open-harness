import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { BundledPlugin, PluginInfo } from '../../shared/extensions'
import { InlineError } from './Feedback'
import { errorMessage } from '../lib/format'
import { ChromeSetup } from './ChromeSetup'

/** Connection and permission controls are composed into the plugin's one card. */
export function BundledPluginSetup({
  plugin,
  installed,
  disabled = false,
}: {
  plugin: BundledPlugin
  installed?: PluginInfo
  disabled?: boolean
}) {
  const client = useQueryClient()
  const catalog = useQuery({
    queryKey: ['bundled-plugins'],
    queryFn: () => window.harness.bundledPlugins(),
    staleTime: 30_000,
    enabled: false,
  })
  const prepare = useMutation({
    mutationFn: () => window.harness.prepareBrowser(),
    onSuccess: () => client.invalidateQueries({ queryKey: ['bundled-plugins'] }),
  })
  const permissions = useMutation({
    mutationFn: (kind: 'accessibility' | 'screen-recording') =>
      window.harness.openComputerSettings(kind),
    onSettled: () => void client.invalidateQueries({ queryKey: ['bundled-plugins'] }),
  })
  const error = prepare.error ?? permissions.error
  return (
    <>
      {plugin.id === 'chrome-use' && plugin.chrome && (
        <ChromeSetup
          connection={plugin.chrome}
          disabled={disabled || !plugin.available}
          installed={installed}
        />
      )}
      {plugin.id === 'browser-use' && plugin.available && !plugin.browserReady && (
        <div className="plugin-description">
          <button
            type="button"
            className="button button-secondary"
            disabled={disabled || prepare.isPending}
            onClick={() => prepare.mutate()}
          >
            {prepare.isPending ? '正在下载浏览器…' : '准备浏览器'}
          </button>
          <small>首次使用需下载 Chromium。安装插件本身不会下载或启动浏览器。</small>
        </div>
      )}
      {plugin.id === 'computer-use' && plugin.available && plugin.desktop && (
        <div className="plugin-description">
          <p>在会话中选择允许操作的应用。点击和输入需要将该应用置于前台，请避免同时操作它。</p>
          <div className="approval-actions">
            <span role="status">
              {plugin.desktop.available === true
                ? '桌面可用'
                : plugin.desktop.available === false
                  ? '桌面暂不可用，请解锁电脑后重试'
                  : '桌面状态检查失败'}
            </span>
            <button
              className="extension-text-button"
              type="button"
              disabled={disabled || catalog.isFetching}
              onClick={() => void catalog.refetch()}
            >
              {catalog.isFetching ? '正在检查…' : '检查状态'}
            </button>
          </div>
          {plugin.desktop.error && <InlineError message={plugin.desktop.error} />}
          <details>
            <summary>使用说明</summary>
            <p>
              Windows 无需开启 macOS 的辅助功能或屏幕录制权限。系统登录、锁屏和 UAC
              安全桌面不能操作；以管理员身份运行的应用也可能无法操作。浏览网页可使用 Chrome
              插件在后台标签页中完成。
            </p>
            <p>当前控制组件：</p>
            <code>{plugin.desktop.helperPath}</code>
          </details>
        </div>
      )}
      {plugin.id === 'computer-use' && plugin.available && !plugin.desktop && (
        <div className="plugin-description">
          <p>为 Open Harness Computer Use 授予 macOS 系统权限后，再在会话中选择允许操作的应用。</p>
          <div className="approval-actions">
            <button
              className="button button-secondary"
              type="button"
              disabled={disabled || permissions.isPending}
              onClick={() => permissions.mutate('accessibility')}
            >
              辅助功能 · {permissionLabel(plugin.permissions?.accessibility)}
            </button>
            <button
              className="button button-secondary"
              type="button"
              disabled={disabled || permissions.isPending}
              onClick={() => permissions.mutate('screen-recording')}
            >
              屏幕录制 · {permissionLabel(plugin.permissions?.screenRecording)}
            </button>
            <button
              className="extension-text-button"
              type="button"
              disabled={disabled || catalog.isFetching || permissions.isPending}
              onClick={() => void catalog.refetch()}
            >
              {catalog.isFetching ? '正在检查…' : '检查权限'}
            </button>
          </div>
          {plugin.permissions?.error && <InlineError message={plugin.permissions.error} />}
          <details>
            <summary>权限设置说明</summary>
            <p>
              在系统列表中选择 Open Harness Computer Use。它是独立的电脑控制组件，主程序仍叫 Open
              Harness。 旧 Open Harness
              条目的授权不会自动转移到新组件；首次使用需要单独开启两项权限。
              授权后返回这里会自动重新检查。本地开发版本更新签名后，系统可能要求重新授权。
            </p>
            <p>当前控制组件：</p>
            <code>{plugin.permissions?.helperPath}</code>
          </details>
        </div>
      )}
      {error && <InlineError message={errorMessage(error)} />}
    </>
  )
}

function permissionLabel(value: boolean | null | undefined): string {
  if (value === true) return '已授权'
  if (value === false) return '未授权'
  return '检查失败'
}
