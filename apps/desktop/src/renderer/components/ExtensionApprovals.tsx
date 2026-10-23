import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ShieldCheck } from 'lucide-react'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import '../extensions.css'

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
    mutationFn: ({ id, allow }: { id: string; allow: boolean }) =>
      window.harness.approveExtension(runId, id, allow),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const error = approve.error ?? query.error
  if (!query.data?.approvals.length && !error) return null
  return (
    <div className="run-extensions">
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
              <p>此服务使用自身权限，可能联网或访问项目外的数据。</p>
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
