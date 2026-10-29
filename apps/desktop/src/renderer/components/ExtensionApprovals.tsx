import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ShieldCheck, Monitor, Square } from 'lucide-react'
import { errorMessage } from '../lib/format'
import { extensionSettingsOptions } from '../lib/extensions'
import { permissionLabel, targetLabel, refreshPermissions } from '../lib/permissions'
import { InlineError } from './Feedback'
import '../extensions.css'
import '../permissions.css'
import type {
  ControlApprovalDecision,
  ApprovalExtent,
  ExtensionApproval,
  ControlScope,
  ControlPermission,
} from '../../shared/extensions'

function capabilityLabel(capability: ControlPermission['capability']): string {
  return capability === 'chrome'
    ? 'Chrome'
    : capability === 'browser'
      ? '独立浏览器'
      : capability === 'computer'
        ? '电脑操作'
        : 'MCP'
}

function controlLabel(scope: ControlScope): string {
  if (scope.kind === 'browser' || scope.kind === 'browser_origin') return '独立浏览器'
  if (scope.kind === 'application') return '电脑操作'
  return 'Chrome'
}

function ApprovalCard({
  approval,
  busy,
  onDecide,
}: {
  approval: ExtensionApproval
  busy: boolean
  onDecide: (id: string, decision: ControlApprovalDecision, extent: ApprovalExtent) => void
}) {
  const [whole, setWhole] = useState(false)
  const extent = whole && approval.can_allow_capability ? 'capability' : 'target'
  const target = approval.permission_target
  const reusable = Boolean(target || approval.can_allow_capability)
  const isChrome =
    approval.session_scope?.kind === 'chrome_tab' ||
    approval.session_scope?.kind === 'chrome_new_tab'
  return (
    <div className="command-approval">
      <div className="approval-heading">
        <ShieldCheck size={15} />
        <strong>
          允许
          {approval.session_scope
            ? `使用${controlLabel(approval.session_scope)}`
            : `调用 ${approval.server_name}`}
          ？
        </strong>
      </div>
      {approval.task_title && (
        <p className="extension-operation">协作任务：{approval.task_title}</p>
      )}
      <p className="extension-operation">{approval.operation}</p>
      {target && <p className="approval-cwd">{targetLabel(target)}</p>}
      <details className="extension-approval-arguments">
        <summary>
          <ChevronDown size={13} />
          查看调用参数
        </summary>
        <pre>{JSON.stringify(approval.arguments, null, 2)}</pre>
      </details>
      <p>
        {extent === 'capability'
          ? `允许本会话及协作任务使用 ${approval.server_name} 的整个能力范围。`
          : target
            ? '允许本会话及协作任务访问上述目标；同一目标的后续操作不再重复询问。'
            : '允许执行这次调用。此服务使用自身权限，可能联网或访问项目外的数据。'}
      </p>
      {isChrome && (
        <p className="extension-hint">在专用标签页分组中操作，你已有的标签页仍需单独共享。</p>
      )}
      {approval.session_scope?.kind === 'application' && (
        <p className="extension-hint">系统授权仍需由你确认。</p>
      )}
      {approval.can_allow_capability && (
        <label className="permission-extent">
          <input
            type="checkbox"
            checked={whole}
            disabled={busy}
            onChange={(event) => setWhole(event.target.checked)}
          />
          扩大到 {approval.server_name} 的所有目标
        </label>
      )}
      {reusable && (
        <p className="extension-hint">
          “始终允许”会保存选定范围，其他会话也可使用；可在设置的授权列表中撤销。
        </p>
      )}
      <div className="approval-actions control-approval-actions">
        <button
          type="button"
          className="extension-text-button"
          disabled={busy}
          onClick={() => onDecide(approval.id, 'cancel', extent)}
        >
          取消
        </button>
        {reusable && (
          <button
            type="button"
            className="button button-secondary"
            disabled={busy}
            onClick={() => onDecide(approval.id, 'always', extent)}
          >
            始终允许
          </button>
        )}
        <button
          type="button"
          className="button button-primary"
          disabled={busy}
          onClick={() => onDecide(approval.id, 'allow', extent)}
        >
          允许
        </button>
      </div>
    </div>
  )
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
    mutationFn: ({
      id,
      decision,
      extent,
    }: {
      id: string
      decision: ControlApprovalDecision
      extent: ApprovalExtent
    }) => window.harness.approveExtension(runId, id, decision, extent),
    onSuccess: (extensions) => {
      client.setQueryData(queryKey, extensions)
      void client.invalidateQueries({ queryKey: extensionSettingsOptions.queryKey })
      refreshPermissions(client)
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
      ...permissions.map((permission) =>
        permission.target ? permissionLabel(permission) : capabilityLabel(permission.capability),
      ),
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
            <ApprovalCard
              key={approval.id}
              approval={approval}
              busy={busy || approve.isPending}
              onDecide={(id, decision, extent) => approve.mutate({ id, decision, extent })}
            />
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
