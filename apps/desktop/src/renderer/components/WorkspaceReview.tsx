import { useState } from 'react'
import {
  FileDiff,
  FolderOpen,
  LoaderCircle,
  RotateCcw,
  ShieldCheck,
  Square,
  Terminal,
  X,
} from 'lucide-react'
import type {
  WorkspaceReview as Review,
  PendingApproval,
  CommandRecord,
  CommandAccess,
  CommandApprovalScope,
  CommandNetworkAccess,
} from '../../shared/contracts'
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

function commandScopeLabel(access?: CommandAccess, network?: CommandNetworkAccess): string {
  return `${access === 'read_only' ? '项目只读' : '可修改项目'} · ${network === 'allowed' ? '允许联网' : '网络关闭'}`
}

export function CommandApprovals({
  approvals,
  busy,
  onDecide,
}: {
  approvals: PendingApproval[]
  busy: boolean
  onDecide: (id: string, allow: boolean, scope?: CommandApprovalScope) => void
}) {
  if (!approvals.length) return null
  return (
    <section className="command-approvals" aria-label="待批准命令">
      {approvals.map((approval) => (
        <div className="command-approval" key={approval.id}>
          <div className="approval-heading">
            <ShieldCheck size={15} />
            <strong>
              {approval.network_access === 'allowed' ? '允许执行并联网？' : '允许执行这条命令？'}
            </strong>
          </div>
          {approval.task_title && <p className="command-task">子任务：{approval.task_title}</p>}
          <pre>{approval.command}</pre>
          <p className="approval-cwd">{approval.cwd}</p>
          <p>
            {approval.execution_mode === 'unavailable'
              ? '当前系统的命令隔离不可用，因此不会执行命令。'
              : approval.execution_mode === 'unrestricted'
                ? '仅本次执行。这条命令将以你的系统账户权限运行，可访问工作目录以外的文件和网络。'
                : `${approval.access === 'read_only' ? '项目只读，文件写入仅限临时目录。' : '可读写工作目录及临时目录。'}${approval.network_access === 'allowed' ? '允许命令及其子进程访问网络。' : '网络关闭。'}`}
          </p>
          {approval.execution_mode === 'sandboxed' && (
            <p>本会话授权适用于主任务及子任务；此范围内的后续命令不再询问，重启应用后失效。</p>
          )}
          <div className="approval-actions">
            <button
              className="button button-secondary"
              disabled={busy}
              onClick={() => onDecide(approval.id, false)}
            >
              拒绝
            </button>
            {approval.execution_mode === 'sandboxed' && (
              <button
                className="button button-secondary"
                disabled={busy}
                onClick={() => onDecide(approval.id, true, 'session')}
              >
                本会话同范围允许
              </button>
            )}
            <button
              className="button button-primary"
              disabled={busy || approval.execution_mode === 'unavailable'}
              onClick={() => onDecide(approval.id, true, 'once')}
            >
              {approval.network_access === 'allowed' ? '允许本次联网执行' : '允许本次'}
            </button>
          </div>
        </div>
      ))}
    </section>
  )
}

export function RunningCommands({
  commands,
  stoppingId,
  onInspect,
  onStop,
}: {
  commands: CommandRecord[]
  stoppingId?: string
  onInspect: () => void
  onStop: (id: string) => void
}) {
  const running = commands.filter((command) => command.status === 'running')
  if (!running.length) return null
  return (
    <section className="running-commands" aria-label="运行中的命令">
      {running.map((command) => (
        <div className="running-command" key={command.id}>
          <LoaderCircle size={14} className="animate-spin" aria-hidden="true" />
          <button className="running-command-inspect" onClick={onInspect} title={command.command}>
            <span className="running-command-label">
              {command.task_title && (
                <span className="command-task truncate">子任务：{command.task_title}</span>
              )}
              <span className="truncate">{command.command}</span>
            </span>
            <small>查看输出</small>
          </button>
          <button
            className="resume-button"
            disabled={stoppingId === command.id}
            onClick={() => onStop(command.id)}
            aria-label={`停止命令：${command.command}`}
          >
            <Square size={12} />
            {stoppingId === command.id ? '正在停止…' : '停止'}
          </button>
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
  onStopCommand,
  stoppingCommandId,
  onRevealWorkspace,
  onRevokeGrants,
  revokingGrants = false,
  initialTab = 'changes',
  managed = false,
}: {
  review?: Review
  loading: boolean
  error?: string
  busy: boolean
  onClose: () => void
  onRefresh: () => void
  onRevert: (path: string) => Promise<unknown>
  onStopCommand: (id: string) => void
  stoppingCommandId?: string
  onRevealWorkspace: () => void
  onRevokeGrants?: () => void
  revokingGrants?: boolean
  initialTab?: 'changes' | 'commands'
  managed?: boolean
}) {
  const [tab, setTab] = useState<'changes' | 'commands'>(initialTab)
  const [confirming, setConfirming] = useState<string | null>(null)
  const title = managed ? '会话文件与命令' : '项目结果'
  return (
    <aside className="workspace-review" aria-label={title}>
      <div className="review-header">
        <strong>{title}</strong>
        <button aria-label={`关闭${title}`} className="review-close" onClick={onClose}>
          <X size={17} />
        </button>
      </div>
      <div className="review-tabs" role="tablist" aria-label={`${title}分类`}>
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
        {review && (
          <button className="review-directory" title={review.root} onClick={onRevealWorkspace}>
            <FolderOpen size={14} />
            打开工作目录
          </button>
        )}
        {error && <InlineError message={error} retry={onRefresh} />}
        {review?.review_incomplete && !review.busy && (
          <InlineError
            message="部分命令改动尚未完成检查，请缩小目录或清理生成文件后重试。"
            retry={onRefresh}
          />
        )}
        {loading && <p className="review-empty">正在读取文件与命令状态…</p>}
        {review && tab === 'changes' && (
          <>
            <p className="review-note">
              {managed ? '文件保存在此会话的工作目录中。' : '改动已保存在项目中。'}
              这里只展示大小限制内的 UTF-8 文本，排除依赖和构建目录。撤销保留此前已有内容。
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
            {!!review.command_grants?.length && (
              <section className="command-grants" aria-label="本会话命令授权">
                <div className="approval-heading">
                  <ShieldCheck size={14} />
                  <strong>本会话命令授权</strong>
                </div>
                <ul>
                  {review.command_grants.map((grant) => (
                    <li key={`${grant.access}-${grant.network_access}`}>
                      {commandScopeLabel(grant.access, grant.network_access)}
                    </li>
                  ))}
                </ul>
                <p className="review-note">
                  主任务及子任务在以上范围内的后续命令不再询问，重启应用后失效。撤销后恢复询问，不会停止已经批准的命令。
                </p>
                <button
                  className="button button-secondary"
                  disabled={revokingGrants || !onRevokeGrants}
                  onClick={onRevokeGrants}
                >
                  {revokingGrants ? '正在撤销…' : '撤销会话授权'}
                </button>
              </section>
            )}
            {review.execution_mode === 'unrestricted' ? (
              <p className="review-note">
                命令经逐次批准后，以你的系统账户权限运行；不限制文件或网络访问。
              </p>
            ) : (
              !review.sandbox_available && (
                <p className="review-note">
                  当前系统的命令隔离不可用，因此不会执行命令。文件工具仍可使用。
                </p>
              )
            )}
            {!review.commands.length && <p className="review-empty">还没有命令记录。</p>}
            {review.commands.map((command) => (
              <details
                className="command-record"
                key={command.id}
                open={command.status === 'running'}
              >
                <summary>
                  <Terminal size={14} />
                  <code>{command.command}</code>
                  <small>
                    {commandLabels[command.status] ?? command.status}
                    {command.exit_code !== null ? ` · ${command.exit_code}` : ''}
                  </small>
                </summary>
                {command.task_title && (
                  <p className="command-record-scope">子任务：{command.task_title}</p>
                )}
                <p className="command-record-scope">
                  请求范围：{command.access === 'read_only' ? '项目只读' : '可修改项目'} ·{' '}
                  {command.network_access === 'allowed' ? '请求联网' : '网络关闭'}
                </p>
                {command.status === 'running' && (
                  <div className="change-actions">
                    <button
                      className="button button-secondary"
                      disabled={stoppingCommandId === command.id}
                      onClick={() => onStopCommand(command.id)}
                    >
                      <Square size={12} />
                      {stoppingCommandId === command.id ? '正在停止…' : '停止命令'}
                    </button>
                    <span>输出自动刷新</span>
                  </div>
                )}
                <pre>{command.output || '暂无输出'}</pre>
              </details>
            ))}
          </>
        )}
      </div>
    </aside>
  )
}
