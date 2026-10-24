import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ChevronRight, Square } from 'lucide-react'
import { collaborationRuns } from '../lib/collaboration'
import { errorMessage } from '../lib/format'
import { runsOptions, storeSnapshot } from '../lib/queries'
import { InlineError } from './Feedback'
import { RunActivity } from './RunActivity'
import '../collaboration.css'

export function CollaborationTasks({ rootId }: { rootId: string }) {
  const client = useQueryClient()
  // The root stream contains only root events. Watch the shared list while this
  // task is active; root stream updates restart polling after an idle period.
  const query = useQuery({
    ...runsOptions,
    refetchInterval: ({ state: { data } }) => {
      if (!data) return 1_000
      return data.some(
        (run) =>
          (run.id === rootId || run.delegation?.root_run_id === rootId) &&
          (run.status === 'running' || run.compacting),
      )
        ? 1_000
        : false
    },
  })
  const cancel = useMutation({
    mutationFn: (id: string) => window.harness.cancelRun(id),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const tasks = collaborationRuns(query.data ?? [], rootId)
  if (!tasks.length && !query.isError) return null
  return (
    <details className="collaboration-tasks">
      <summary>
        <ChevronRight size={13} className="collaboration-chevron" aria-hidden="true" />
        <span>协作任务</span>
        <span className="collaboration-count">{tasks.length || ''}</span>
        {tasks.some((task) => task.status === 'running' || task.compacting) && (
          <RunActivity active />
        )}
      </summary>
      {query.isError && (
        <InlineError message="协作任务刷新失败。" retry={() => void query.refetch()} />
      )}
      {cancel.isError && <InlineError message={errorMessage(cancel.error)} />}
      <ul className="collaboration-list" aria-label="协作任务">
        {tasks.map((task) => {
          const name = task.delegation!.task_name || task.title || task.prompt
          const active = task.status === 'running' || Boolean(task.compacting)
          return (
            <li key={task.id}>
              <Link
                to="/runs/$runId"
                params={{ runId: task.id }}
                className="collaboration-task-link"
                title={`查看协作任务：${name}`}
              >
                <RunActivity active={active} />
                <span className="truncate">{name}</span>
                <ChevronRight size={12} aria-hidden="true" />
              </Link>
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
