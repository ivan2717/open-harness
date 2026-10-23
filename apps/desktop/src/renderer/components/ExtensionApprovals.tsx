import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ShieldCheck, Monitor, Square } from 'lucide-react'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import '../extensions.css'
import type { ControlScope } from '../../shared/extensions'

function controlLabel(scope: ControlScope): string {
  if (scope.kind === 'browser') return '独立浏览器'
  if (scope.kind === 'application') return scope.bundle_id
  return `Chrome · ${scope.origin}${scope.kind === 'chrome_tab' ? ` · 标签页 ${scope.tab_id}` : ''}`
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
    mutationFn: ({ id, allow, session }: { id: string; allow: boolean; session?: boolean }) =>
      window.harness.approveExtension(runId, id, allow, session),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const revoke = useMutation({
    mutationFn: () => window.harness.revokeControl(runId),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const error = approve.error ?? revoke.error ?? query.error
  if (!query.data?.approvals.length && !query.data?.controls?.length && !error) return null
  return (
    <div className="run-extensions">
      {!!query.data?.controls?.length && (
        <section className="control-session" aria-label="电脑与浏览器控制">
          <Monitor size={15} />
          <span>
            {query.data.controls.map((control) => controlLabel(control.scope)).join(' · ')}
            <small>
              {query.data.controls.every((control) => control.remembered)
                ? '本会话控制'
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
                <strong>允许调用 {approval.server_name}？</strong>
              </div>
              <p className="extension-operation">{approval.operation}</p>
              <details className="extension-approval-arguments">
                <summary>
                  <ChevronDown size={13} />
                  查看调用参数
                </summary>
                <pre>{JSON.stringify(approval.arguments, null, 2)}</pre>
              </details>
              <p>
                {approval.session_scope?.kind === 'browser'
                  ? '使用独立浏览器。会话授权允许连续浏览和操作，直到你停止控制或关闭应用。'
                  : approval.session_scope?.kind === 'application'
                    ? `控制应用 ${approval.session_scope.bundle_id}（进程 ${approval.session_scope.pid}）。首次控制其他应用时会另行请求授权。`
                    : approval.session_scope?.kind === 'chrome_tab'
                      ? `在后台操作 Chrome 标签页 ${approval.session_scope.tab_id}，网站 ${approval.session_scope.origin}。其他标签页和网站需另行授权。`
                      : approval.session_scope?.kind === 'chrome_new_tab'
                        ? `在 ${approval.session_scope.origin} 新建一个后台标签页。会话授权仅适用于这个新标签页和网站。`
                        : '此服务使用自身权限，可能联网或访问项目外的数据。'}
              </p>
              <div className="approval-actions">
                <button
                  type="button"
                  className="button button-secondary"
                  disabled={busy || approve.isPending}
                  onClick={() => approve.mutate({ id: approval.id, allow: false })}
                >
                  拒绝
                </button>
                <button
                  type="button"
                  className="button button-primary"
                  disabled={busy || approve.isPending}
                  onClick={() => approve.mutate({ id: approval.id, allow: true })}
                >
                  允许本次
                </button>
                {approval.session_scope && (
                  <button
                    type="button"
                    className="button button-primary"
                    disabled={busy || approve.isPending}
                    onClick={() => approve.mutate({ id: approval.id, allow: true, session: true })}
                  >
                    允许本会话
                  </button>
                )}
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
