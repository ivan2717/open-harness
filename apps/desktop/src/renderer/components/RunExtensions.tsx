import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { BookOpen, ChevronDown, RefreshCw, ShieldCheck } from 'lucide-react'
import { errorMessage } from '../lib/format'
import { useSaveExtensions, withSkillEnabled } from '../lib/extensions'
import { InlineError } from './Feedback'
import { SkillDiagnostics, SkillList } from './SkillList'
import '../extensions.css'

export function RunExtensions({
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
  const save = useSaveExtensions()
  const approve = useMutation({
    mutationFn: ({ id, allow }: { id: string; allow: boolean }) =>
      window.harness.approveExtension(runId, id, allow),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
    onSettled: () => void client.invalidateQueries({ queryKey }),
  })
  const refresh = useMutation({
    mutationFn: () => window.harness.refreshRunSkills(runId),
    onSuccess: (extensions) => client.setQueryData(queryKey, extensions),
  })
  const error = approve.error ?? save.error ?? refresh.error ?? query.error
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
      <details className="run-skills">
        <summary>
          <BookOpen size={13} />
          技能
          {query.data?.skills.length
            ? ` · ${query.data.skills.filter((skill) => skill.enabled).length} 个已启用`
            : ''}
          <ChevronDown size={12} />
        </summary>
        <div className="run-skills-content">
          <div className="extension-section-heading">
            <span>本会话可用的技能</span>
            <button
              type="button"
              className="extension-text-button"
              disabled={refresh.isPending}
              onClick={() => refresh.mutate()}
            >
              <RefreshCw size={13} className={refresh.isPending ? 'animate-spin' : undefined} />
              刷新
            </button>
          </div>
          {query.isPending && <p className="extension-hint">正在读取技能…</p>}
          {query.data && (
            <SkillList
              skills={query.data.skills}
              busy={save.isPending}
              onToggle={(id, enabled) =>
                save.mutate((current) => withSkillEnabled(current, id, enabled))
              }
            />
          )}
          {query.data?.skills.length === 0 && (
            <p className="extension-hint">
              还没有可用技能。在设置中安装个人技能，或将技能放入项目的 .agents/skills 文件夹。
            </p>
          )}
          {query.data && <SkillDiagnostics diagnostics={query.data.diagnostics} />}
        </div>
      </details>
    </div>
  )
}
