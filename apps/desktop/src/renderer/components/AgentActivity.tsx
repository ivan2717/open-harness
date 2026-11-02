import type { ReactNode } from 'react'
import { ChevronRight, CircleAlert, LoaderCircle, Users } from 'lucide-react'
import type { RunStatus, RunSummary } from '../../shared/contracts'
import '../agent-activity.css'

const statuses: Record<RunStatus, string> = {
  running: '运行中',
  waiting_for_input: '等待输入',
  completed: '已完成',
  failed: '失败',
  cancelled: '已停止',
  interrupted: '已中断',
  paused: '已暂停',
}

/** An activity points to a known child run; its current state comes from that run. */
export function AgentActivity({
  targets,
  label,
  selectedId,
  openAgent,
  children,
}: {
  targets: RunSummary[]
  label: string
  selectedId: string | null
  openAgent: (id: string) => void
  children: ReactNode
}) {
  return (
    <div className="agent-activity">
      {targets.map((target) => {
        const name = (target.delegation?.task_name || target.title).replace(/[_-]+/g, ' ')
        const active = target.status === 'running' || Boolean(target.compacting)
        const status = target.compacting ? '整理上下文' : statuses[target.status]
        const failed = target.status === 'failed'
        return (
          <button
            key={target.id}
            type="button"
            className={`agent-activity-link${selectedId === target.id ? ' is-selected' : ''}${failed ? ' is-failed' : ''}`}
            aria-label={`查看协作任务：${name}`}
            aria-pressed={selectedId === target.id}
            title={`${name} · ${label} · ${status}`}
            onClick={() => openAgent(target.id)}
          >
            <span className="agent-activity-icon" aria-hidden="true">
              {active ? (
                <LoaderCircle size={13} className="animate-spin" />
              ) : failed ? (
                <CircleAlert size={13} />
              ) : (
                <Users size={13} />
              )}
            </span>
            <span className="agent-activity-name">{name}</span>
            <span className="agent-activity-operation">{label}</span>
            <span className="agent-activity-status">{status}</span>
            <ChevronRight size={12} className="agent-activity-chevron" aria-hidden="true" />
          </button>
        )
      })}
      {children}
    </div>
  )
}
