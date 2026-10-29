import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ControlPermission } from '../../shared/extensions'
import { errorMessage } from '../lib/format'
import { permissionOptions, refreshPermissions } from '../lib/permissions'
import { runsOptions } from '../lib/queries'
import { RunPermissionsDialog } from './RunPermissionsDialog'
import { PermissionList } from './PermissionList'
import { InlineError } from './Feedback'
import '../permissions.css'
export { PermissionList } from './PermissionList'

function SavedPermissionSettings() {
  const client = useQueryClient()
  const query = useQuery(permissionOptions)
  const revoke = useMutation({
    mutationFn: (permission: ControlPermission) => window.harness.revokePermission(permission),
    onSuccess: () => refreshPermissions(client),
  })
  return (
    <section className="permission-settings-section">
      <h3>始终允许的访问</h3>
      <p>查看和撤销已保存的网站、应用及 MCP 授权。撤销将停止该服务当前控制，其他目标授权保留。</p>
      {query.isPending ? (
        <p role="status">正在读取授权…</p>
      ) : query.data?.length ? (
        <PermissionList
          permissions={query.data}
          busy={revoke.isPending}
          onRevoke={(permission) => revoke.mutate(permission)}
        />
      ) : (
        !query.error && <p className="settings-footnote">尚未保存长期授权。</p>
      )}
      {(query.error || revoke.error) && (
        <InlineError
          message={errorMessage(query.error ?? revoke.error)}
          retry={() => void query.refetch()}
        />
      )}
    </section>
  )
}

function ConversationPermissionSettings() {
  const runs = useQuery(runsOptions)
  const [selected, setSelected] = useState('')
  const [open, setOpen] = useState(false)
  const roots = runs.data?.filter((run) => !run.delegation) ?? []
  const runId = roots.some((run) => run.id === selected) ? selected : (roots[0]?.id ?? '')
  return (
    <section className="permission-settings-section">
      <h3>会话授权记录</h3>
      <p>查看命令和工具授权、自动审查记录，或收回某个会话的权限。</p>
      <div className="permission-session-picker">
        <select
          aria-label="选择要管理授权的会话"
          value={runId}
          onChange={(event) => setSelected(event.target.value)}
          disabled={!roots.length}
        >
          {!roots.length && <option value="">暂无会话</option>}
          {roots.map((run) => (
            <option key={run.id} value={run.id}>
              {run.title}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="button button-secondary"
          disabled={!runId}
          onClick={() => setOpen(true)}
        >
          查看授权记录
        </button>
      </div>
      {runs.error && (
        <InlineError message={errorMessage(runs.error)} retry={() => void runs.refetch()} />
      )}
      {open && runId && <RunPermissionsDialog runId={runId} onClose={() => setOpen(false)} />}
    </section>
  )
}

export function PermissionSettings() {
  const [expanded, setExpanded] = useState(false)
  return (
    <section className="settings-section">
      <h2>权限</h2>
      <details
        className="permission-settings-details"
        onToggle={(event) => setExpanded(event.currentTarget.open)}
      >
        <summary>管理授权与审查记录</summary>
        {expanded && (
          <>
            <ConversationPermissionSettings />
            <SavedPermissionSettings />
          </>
        )}
      </details>
    </section>
  )
}
