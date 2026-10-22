import { useState } from 'react'
import { FileDiff, RotateCcw, ShieldCheck, Terminal, X } from 'lucide-react'
import type { WorkspaceReview as Review, PendingApproval } from '../../shared/contracts'
import { InlineError } from './Feedback'

const changeLabels: Record<string, string> = {
  added: '新增',
  deleted: '删除',
  modified: '修改',
  externally_modified: '已有其他修改',
  unavailable: '无法读取',
}
const commandLabels: Record<string, string> = {
  awaiting_approval: '等待批准',
  running: '执行中',
  completed: '已完成',
  failed: '执行失败',
  denied: '已拒绝',
  cancelled: '已停止',
  timed_out: '已超时',
  interrupted: '已中断',
}

export function CommandApprovals({
  approvals,
  busy,
  onDecide,
}: {
  approvals: PendingApproval[]
  busy: boolean
  onDecide: (id: string, allow: boolean) => void
}) {
  if (!approvals.length) return null
  return (
    <section className="command-approvals" aria-label="待批准命令">
      {approvals.map((approval) => (
        <div className="command-approval" key={approval.id}>
          <div className="approval-heading">
            <ShieldCheck size={15} />
            <strong>允许执行这条命令？</strong>
          </div>
          <pre>{approval.command}</pre>
          <p className="approval-cwd">{approval.cwd}</p>
          <p>仅本次执行；网络关闭，写入限制在项目目录和临时目录。</p>
          <div className="approval-actions">
            <button
              className="button button-secondary"
              disabled={busy}
              onClick={() => onDecide(approval.id, false)}
            >
              拒绝
            </button>
            <button
              className="button button-primary"
              disabled={busy}
              onClick={() => onDecide(approval.id, true)}
            >
              允许本次
            </button>
          </div>
        </div>
      ))}
    </section>
  )
}

export function WorkspaceReviewPanel({
  review,
  loading,
  error,
  busy,
  onClose,
  onRefresh,
  onRevert,
}: {
  review?: Review
  loading: boolean
  error?: string
  busy: boolean
  onClose: () => void
  onRefresh: () => void
  onRevert: (path: string) => Promise<unknown>
}) {
  const [tab, setTab] = useState<'changes' | 'commands'>('changes')
  const [confirming, setConfirming] = useState<string | null>(null)
  return (
    <aside className="workspace-review" aria-label="项目结果">
      <div className="review-header">
        <strong>项目结果</strong>
        <button aria-label="关闭项目结果" className="review-close" onClick={onClose}>
          <X size={17} />
        </button>
      </div>
      <div className="review-tabs" role="tablist" aria-label="项目结果分类">
        <button role="tab" aria-selected={tab === 'changes'} onClick={() => setTab('changes')}>
          <FileDiff size={14} />
          改动 {review?.changes.length ?? 0}
        </button>
        <button role="tab" aria-selected={tab === 'commands'} onClick={() => setTab('commands')}>
          <Terminal size={14} />
          命令 {review?.commands.length ?? 0}
        </button>
        <button className="review-refresh" aria-label="刷新项目结果" onClick={onRefresh}>
          <RotateCcw size={13} />
        </button>
      </div>
      <div className="review-content">
        {error && <InlineError message={error} retry={onRefresh} />}
        {review?.review_incomplete && !review.busy && (
          <InlineError
            message="部分命令改动尚未完成检查，请缩小目录或清理生成文件后重试。"
            retry={onRefresh}
          />
        )}
        {loading && <p className="review-empty">正在读取项目状态…</p>}
        {review && tab === 'changes' && (
          <>
            <p className="review-note">
              改动已保存在项目中。这里只展示大小限制内的 UTF-8
              文本，排除依赖和构建目录。撤销保留此前已有内容。
            </p>
            {!review.changes.length && <p className="review-empty">未检测到可展示的文本改动。</p>}
            {review.changes.map((change) => (
              <details className="file-change" key={change.path}>
                <summary>
                  <FileDiff size={14} />
                  <span>{change.path}</span>
                  <small>{changeLabels[change.status] ?? change.status}</small>
                </summary>
                <div className="change-actions">
                  {change.revertible ? (
                    confirming === change.path ? (
                      <>
                        <span>撤销此文件的本会话改动？</span>
                        <button
                          className="button button-secondary"
                          disabled={busy || review.busy}
                          onClick={() => setConfirming(null)}
                        >
                          保留
                        </button>
                        <button
                          className="button button-secondary"
                          disabled={busy || review.busy}
                          onClick={async () => {
                            try {
                              await onRevert(change.path)
                              setConfirming(null)
                            } catch {
                              /* The mutation displays the error. */
                            }
                          }}
                        >
                          确认撤销
                        </button>
                      </>
                    ) : (
                      <button
                        className="button button-secondary"
                        disabled={busy || review.busy}
                        onClick={() => setConfirming(change.path)}
                      >
                        <RotateCcw size={12} />
                        撤销此文件
                      </button>
                    )
                  ) : (
                    <span>无法安全自动撤销，请检查后手动处理。</span>
                  )}
                </div>
                <div className="file-version file-before">
                  <h3>修改前</h3>
                  <pre>{change.before ?? '（文件不存在或无法显示）'}</pre>
                </div>
                <div className="file-version file-after">
                  <h3>当前内容</h3>
                  <pre>{change.after ?? '（文件不存在或无法显示）'}</pre>
                </div>
              </details>
            ))}
          </>
        )}
        {review && tab === 'commands' && (
          <>
            {!review.sandbox_available && (
              <p className="review-note">
                当前系统的命令隔离不可用，因此不会执行命令。文件工具仍可使用。
              </p>
            )}
            {!review.commands.length && <p className="review-empty">还没有命令记录。</p>}
            {review.commands.map((command) => (
              <details className="command-record" key={command.id}>
                <summary>
                  <Terminal size={14} />
                  <code>{command.command}</code>
                  <small>
                    {commandLabels[command.status] ?? command.status}
                    {command.exit_code !== null ? ` · ${command.exit_code}` : ''}
                  </small>
                </summary>
                <pre>{command.output || '暂无输出'}</pre>
              </details>
            ))}
          </>
        )}
      </div>
    </aside>
  )
}
