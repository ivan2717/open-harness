import { useEffect, useId, useRef } from 'react'
import { createPortal } from 'react-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { X } from 'lucide-react'
import type { ControlPermission } from '../../shared/extensions'
import { errorMessage } from '../lib/format'
import { commandGrantLabel, refreshPermissions, runPermissionOptions } from '../lib/permissions'
import { InlineError } from './Feedback'
import { PermissionList } from './PermissionList'
import '../permissions.css'

const reviewLabels = {
  reviewing: '正在审查',
  approved: '已批准',
  manual: '需要你确认',
  cancelled: '已取消',
}

export function RunPermissionsDialog({ runId, onClose }: { runId: string; onClose: () => void }) {
  const id = useId()
  const dialog = useRef<HTMLDialogElement>(null)
  const client = useQueryClient()
  const query = useQuery({ ...runPermissionOptions(runId), refetchInterval: 2_000 })
  const workspace = useQuery({
    queryKey: ['workspace', runId],
    queryFn: () => window.harness.getWorkspace(runId),
    retry: 1,
  })
  const extensions = useQuery({
    queryKey: ['run-extensions', runId],
    queryFn: () => window.harness.runExtensions(runId),
    refetchInterval: 2_000,
  })
  const revoke = useMutation({
    mutationFn: (stop: boolean) => window.harness.revokeRunPermissions(runId, stop),
    onSuccess: () => refreshPermissions(client),
  })
  const revokeTarget = useMutation({
    mutationFn: (permission: ControlPermission) =>
      window.harness.revokePermission(
        permission,
        permission.lifetime === 'always' ? undefined : runId,
      ),
    onSuccess: () => refreshPermissions(client),
  })
  const busy = revoke.isPending || revokeTarget.isPending
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      element?.close()
      if (previous?.isConnected) previous.focus()
    }
  }, [])
  const error = query.error ?? revoke.error ?? revokeTarget.error
  const fullAccess = query.data?.approval_mode === 'full_access'
  const persistent = query.data?.persistent_command_grants ?? []
  const session = workspace.data?.command_grants ?? []
  const grants = [
    ...new Map(
      [...session, ...persistent].map((grant) => [commandGrantLabel(grant), grant]),
    ).values(),
  ]
  return createPortal(
    <dialog
      ref={dialog}
      className="permission-dialog"
      aria-labelledby={id}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
    >
      <header>
        <h2 id={id}>会话授权记录</h2>
        <button
          type="button"
          className="icon-button"
          aria-label="关闭会话授权记录"
          onClick={onClose}
        >
          <X size={17} />
        </button>
      </header>
      <p className="permission-hint">适用于主任务及其协作任务。</p>
      {query.isPending && <p role="status">正在读取权限…</p>}
      {fullAccess && (
        <div className="permission-full-access-notice">
          <p>当前会话使用完全访问权限。仅删除授权记录不会限制访问；收回权限后将恢复请求批准。</p>
          <button
            type="button"
            className="button button-secondary"
            disabled={busy}
            onClick={() => revoke.mutate(true)}
          >
            收回权限并停止任务
          </button>
        </div>
      )}
      <section className="permission-section">
        <h3>已批准的访问</h3>
        {!!grants.length && (
          <ul className="permission-list">
            {grants.map((grant) => (
              <li key={commandGrantLabel(grant)}>
                <div>
                  <strong>命令 · {commandGrantLabel(grant)}</strong>
                  <small>
                    {persistent.some((item) => commandGrantLabel(item) === commandGrantLabel(grant))
                      ? '本会话及协作任务 · 重启后保留'
                      : '本会话及协作任务 · 重启后失效'}
                  </small>
                </div>
              </li>
            ))}
          </ul>
        )}
        <p className="permission-hint">逐项撤销将停止该服务当前控制，其他目标授权保留。</p>
        <PermissionList
          permissions={extensions.data?.control_permissions ?? []}
          busy={busy}
          onRevoke={(permission) => revokeTarget.mutate(permission)}
        />
        {!grants.length && !extensions.data?.control_permissions?.length && (
          <p className="permission-hint">暂无额外授权记录。</p>
        )}
        {(workspace.error || extensions.error) && (
          <InlineError
            message={`授权读取失败：${errorMessage(workspace.error ?? extensions.error)}`}
            retry={() => {
              void workspace.refetch()
              void extensions.refetch()
            }}
          />
        )}
        {!fullAccess && (
          <p className="permission-hint">
            撤销会清除本会话的命令授权和控制许可，正在执行的命令不会中止；跨会话的“始终允许”请逐项撤销。
          </p>
        )}
        {!fullAccess && (
          <div className="permission-actions">
            <button
              type="button"
              className="button button-secondary"
              disabled={busy}
              onClick={() => revoke.mutate(false)}
            >
              撤销会话授权
            </button>
            <button
              type="button"
              className="permission-text-button"
              disabled={busy}
              onClick={() => revoke.mutate(true)}
            >
              撤销并停止任务
            </button>
          </div>
        )}
      </section>
      {!!query.data?.reviews.length && (
        <section className="permission-section">
          <h3>自动审查记录</h3>
          <ul className="permission-reviews">
            {[...query.data.reviews].reverse().map((review) => (
              <li key={`${review.run_id}:${review.id}`}>
                <details open={review.status === 'manual' || review.status === 'reviewing'}>
                  <summary>
                    <span>{reviewLabels[review.status]}</span>
                    <time>{new Date(review.created_at).toLocaleString()}</time>
                  </summary>
                  <code>{review.operation}</code>
                  <p>{review.reason}</p>
                  <small>
                    {review.model_id}
                    {review.usage
                      ? ` · 输入 ${review.usage.input_tokens} / 输出 ${review.usage.output_tokens} tokens`
                      : ''}
                  </small>
                </details>
              </li>
            ))}
          </ul>
        </section>
      )}
      {error && <InlineError message={errorMessage(error)} retry={() => void query.refetch()} />}
    </dialog>,
    document.body,
  )
}
