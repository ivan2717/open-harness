import type { RunDetail, RunSummary } from '../../shared/contracts'

/** Keep a task's delegated work together without mixing unrelated conversations. */
export function collaborationRuns(runs: RunSummary[], rootId: string): RunSummary[] {
  return runs
    .filter((run) => run.id !== rootId && run.delegation?.root_run_id === rootId)
    .sort(
      (left, right) =>
        left.created_at.localeCompare(right.created_at) || left.id.localeCompare(right.id),
    )
}

export function pauseMessage(run: RunDetail): string {
  for (let index = run.events.length - 1; index >= 0; index -= 1) {
    const event = run.events[index]
    if (event.type === 'run_paused') return event.detail || '任务已暂停，进度已保存。'
  }
  return '任务已暂停，进度已保存。'
}
