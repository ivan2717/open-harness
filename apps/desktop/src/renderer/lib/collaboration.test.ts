import { describe, expect, it } from 'vitest'
import type { RunDetail, RunSummary } from '../../shared/contracts'
import { collaborationRuns, pauseMessage } from './collaboration'

function task(id: string, rootId?: string, parentId = rootId): RunSummary {
  return {
    id,
    revision: 1,
    title: id,
    prompt: id,
    model_id: 'test',
    status: 'running',
    created_at: '2026-10-24T08:00:00Z',
    updated_at: '2026-10-24T08:00:00Z',
    ...(rootId
      ? {
          delegation: {
            root_run_id: rootId,
            parent_run_id: parentId!,
            task_name: id,
            write_paths: [],
          },
        }
      : {}),
  }
}

describe('collaboration task grouping', () => {
  it('includes nested children of the root and excludes unrelated or ordinary runs', () => {
    const runs = [
      task('root'),
      task('child', 'root'),
      task('nested', 'root', 'child'),
      task('other', 'elsewhere'),
    ]
    expect(collaborationRuns(runs, 'root').map((run) => run.id)).toEqual(['child', 'nested'])
    expect(collaborationRuns(runs, 'child')).toEqual([])
  })

  it('keeps creation order stable when a child receives updates', () => {
    const first = task('first', 'root')
    const second = { ...task('second', 'root'), created_at: '2026-10-24T08:01:00Z' }
    const runs = [second, { ...first, updated_at: '2026-10-24T08:02:00Z' }]
    expect(collaborationRuns(runs, 'root').map((run) => run.id)).toEqual(['first', 'second'])
    expect(runs[0]).toBe(second)
  })

  it('uses the latest pause reason and falls back for old snapshots', () => {
    const run: RunDetail = {
      ...task('root'),
      status: 'paused',
      messages: [],
      context: null,
      events: [],
    }
    expect(pauseMessage(run)).toBe('任务已暂停，进度已保存。')
    run.events = ['第一次暂停', '已达到本次执行预算，可以继续。'].map((detail, sequence) => ({
      sequence,
      detail,
      type: 'run_paused',
      step: 1,
      label: '已暂停',
      timestamp: run.updated_at,
    }))
    expect(pauseMessage(run)).toBe('已达到本次执行预算，可以继续。')
  })
})
