import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, Square } from 'lucide-react'
import type { RunSummary } from '../../shared/contracts'
import { errorMessage } from '../lib/format'
import { storeSnapshot } from '../lib/queries'
import { InlineError } from './Feedback'
import { RunActivity } from './RunActivity'
import '../collaboration.css'

export function CollaborationTasks({
  tasks,
  onOpen,
  error,
  onRetry,
}: {
  tasks: RunSummary[]
  onOpen: (id: string) => void
  error?: boolean
  onRetry: () => void
}) {
  const client = useQueryClient()
  const cancel = useMutation({
    mutationFn: (id: string) => window.harness.cancelRun(id),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  if (!tasks.length && !error) return null
  return (
    <details className="collaboration-tasks">
      <summary>
        <ChevronRight size={13} className="collaboration-chevron" aria-hidden="true" />
        <span>子智能体</span>
        <span className="collaboration-count">{tasks.length || ''}</span>
        {tasks.some((task) => task.status === 'running' || task.compacting) && (
          <RunActivity active />
        )}
      </summary>
      {error && <InlineError message="子智能体状态刷新失败。" retry={onRetry} />}
      {cancel.isError && <InlineError message={errorMessage(cancel.error)} />}
      <ul className="collaboration-list" aria-label="协作任务">
        {tasks.map((task) => {
          const name = task.delegation!.task_name || task.title || task.prompt
          const active = task.status === 'running' || Boolean(task.compacting)
          return (
            <li key={task.id}>
              <button
                type="button"
                onClick={() => onOpen(task.id)}
                className="collaboration-task-link"
                title={`查看协作任务：${name}`}
              >
                <RunActivity active={active} />
                <span className="truncate">{name}</span>
                <ChevronRight size={12} aria-hidden="true" />
              </button>
              {task.status === 'running' && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label={`停止协作任务：${name}`}
                  title="停止任务"
                  disabled={cancel.isPending}
                  onClick={() => cancel.mutate(task.id)}
                >
                  <Square size={12} aria-hidden="true" />
                </button>
              )}
            </li>
          )
        })}
      </ul>
    </details>
  )
}
