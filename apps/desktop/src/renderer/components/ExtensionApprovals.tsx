import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ShieldCheck, Monitor, Square } from 'lucide-react'
import { errorMessage } from '../lib/format'
import { extensionSettingsOptions } from '../lib/extensions'
import { InlineError } from './Feedback'
import '../extensions.css'
import type {
  ControlApprovalDecision,
  ControlScope,
  ControlPermission,
} from '../../shared/extensions'

function capabilityLabel(capability: ControlPermission['capability']): string {
  return capability === 'chrome' ? 'Chrome' : capability === 'browser' ? '独立浏览器' : '电脑操作'
}

function controlLabel(scope: ControlScope): string {
  if (scope.kind === 'browser') return '独立浏览器'
  if (scope.kind === 'application') return '电脑操作'
  return 'Chrome'
}

function controlDescription(scope: ControlScope): string {
  if (scope.kind === 'application')
    return '允许本会话及协作任务使用电脑操作，选择和切换已运行的应用。系统授权仍需由你确认。'
  if (scope.kind === 'browser') return '允许本会话及协作任务使用独立浏览器，连续打开网页和操作。'
  return '允许本会话及协作任务在专用标签页分组中使用 Chrome。你已有的标签页仍需单独共享。'
}

export function ExtensionApprovals({
  runId,
  running,
  busy = false,
}: {
  runId: string
  running: boolean
  busy?: boolean
}) {
  const client = useQueryClient()
  const queryKey = ['run-extensions', runId]
  const query = useQuery({
    queryKey,
    queryFn: () => window.harness.runExtensions(runId),
    refetchInterval: running ? 1_000 : 5_000,
    retry: 1,
  })
  const approve = useMutation({
    mutationFn: ({ id, decision }: { id: string; decision: ControlApprovalDecision }) =>
      window.harness.approveExtension(runId, id, decision),
    onSuccess: (extensions) => {
      client.setQueryData(queryKey, extensions)
      void client.invalidateQueries({ queryKey: extensionSettingsOptions.queryKey })
    },
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const revoke = useMutation({
    mutationFn: () => window.harness.revokeControl(runId),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const error = approve.error ?? revoke.error ?? query.error
  const controls = query.data?.controls ?? []
  const permissions = query.data?.control_permissions ?? []
  const controlLabels = [
    ...new Set([
      ...permissions.map((permission) => capabilityLabel(permission.capability)),
      ...controls.map((control) => controlLabel(control.scope)),
    ]),
  ]
  if (!query.data?.approvals.length && !controlLabels.length && !error) return null
  return (
    <div className="run-extensions">
      {!!controlLabels.length && (
        <section className="control-session" aria-label="电脑与浏览器控制">
          <Monitor size={15} />
          <span>
            {controlLabels.join(' · ')}
            <small>
              {controls.every((control) => control.remembered)
                ? '本会话及协作任务控制'
                : '当前控制范围'}{' '}
              · 可随时停止
            </small>
          </span>
          <button
            type="button"
            className="button button-secondary"
            disabled={revoke.isPending}
            onClick={() => revoke.mutate()}
          >
            <Square size={12} />
            停止控制
          </button>
        </section>
      )}
      {(query.data?.approvals.length ?? 0) > 0 && (
        <section className="command-approvals" aria-label="待批准外部工具">
          {query.data!.approvals.map((approval) => (
            <div className="command-approval" key={approval.id}>
              <div className="approval-heading">
                <ShieldCheck size={15} />
                <strong>
                  {approval.session_scope
                    ? `允许使用${controlLabel(approval.session_scope)}？`
                    : `允许调用 ${approval.server_name}？`}
                </strong>
              </div>
              {approval.task_title && (
                <p className="extension-operation">协作任务：{approval.task_title}</p>
              )}
              <p className="extension-operation">{approval.operation}</p>
              <details className="extension-approval-arguments">
                <summary>
                  <ChevronDown size={13} />
                  查看调用参数
                </summary>
                <pre>{JSON.stringify(approval.arguments, null, 2)}</pre>
              </details>
              <p>
                {approval.session_scope
                  ? controlDescription(approval.session_scope)
                  : '允许执行这次调用。此服务使用自身权限，可能联网或访问项目外的数据。'}
              </p>
              {approval.session_scope && (
                <p className="extension-hint">
                  选择「始终允许」后，其他会话也可使用；可在插件设置中取消。
                </p>
              )}
              <div className="approval-actions control-approval-actions">
                <button
                  type="button"
                  className="extension-text-button"
                  disabled={busy || approve.isPending}
                  onClick={() => approve.mutate({ id: approval.id, decision: 'cancel' })}
                >
                  取消
                </button>
                {approval.session_scope && (
                  <button
                    type="button"
                    className="button button-secondary"
                    disabled={busy || approve.isPending}
                    onClick={() => approve.mutate({ id: approval.id, decision: 'always' })}
                  >
                    始终允许
                  </button>
                )}
                <button
                  type="button"
                  className="button button-primary"
                  disabled={busy || approve.isPending}
                  onClick={() => approve.mutate({ id: approval.id, decision: 'allow' })}
                >
                  允许
                </button>
              </div>
            </div>
          ))}
        </section>
      )}
      {error && (
        <InlineError
          message={`扩展状态：${errorMessage(error)}`}
          retry={() => void query.refetch()}
        />
      )}
    </div>
  )
}
