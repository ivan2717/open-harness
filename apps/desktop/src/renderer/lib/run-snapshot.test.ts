import { describe, expect, it } from 'vitest'
import type { RunDetail } from '../../shared/contracts'
import { latestSnapshot, mergeRunLists, updateRunList } from './run-snapshot'

function snapshot(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: 'run-1',
    revision: 1,
    title: 'Context test',
    prompt: 'Test',
    status: 'running',
    model_id: 'local-demo',
    created_at: '2026-10-21T12:00:00Z',
    updated_at: '2026-10-21T12:00:00Z',
    messages: [],
    events: [],
    context: null,
    ...overrides,
  }
}

describe('snapshot ordering', () => {
  it('keeps a newer stream update when an older HTTP response arrives later', () => {
    const streamed = snapshot({
      revision: 9,
      messages: [{ id: 'reply', role: 'assistant', text: '完整回答', status: 'complete' }],
      status: 'completed',
    })
    expect(latestSnapshot(streamed, snapshot({ revision: 8 }))).toBe(streamed)
    expect(latestSnapshot(streamed, snapshot({ revision: 9 }))).toBe(streamed)
  })

  it('accepts a new revision even when text is shorter or timestamps are equal', () => {
    const before = snapshot({
      revision: 3,
      messages: [{ id: 'partial', role: 'assistant', text: '未提交的输出', status: 'streaming' }],
    })
    const after = snapshot({ revision: 4, status: 'cancelled', messages: [] })
    expect(latestSnapshot(before, after)).toBe(after)
  })

  it('treats run IDs as independent revision scopes', () => {
    const next = snapshot({ id: 'run-2', revision: 1 })
    expect(latestSnapshot(snapshot({ revision: 50 }), next)).toBe(next)
    expect(latestSnapshot(undefined, next)).toBe(next)
  })
})

describe('session list updates', () => {
  it('retains child identity and execution policy across streaming and pause updates', () => {
    const child = snapshot({
      delegation: {
        parent_run_id: 'root',
        root_run_id: 'root',
        task_name: 'Review',
        write_paths: [],
      },
      execution_policy: { max_steps: 200, max_agents: 4 },
    })
    const paused = { ...child, revision: 2, status: 'paused' as const }
    const list = updateRunList(updateRunList([], child), paused)
    expect(list[0].delegation).toEqual(child.delegation)
    expect(list[0].execution_policy).toEqual(child.execution_policy)
    expect(mergeRunLists(list, [child])[0].status).toBe('paused')
  })

  it('keeps one summary per run without copying detail blocks into the list cache', () => {
    const run = snapshot({
      execution_directory: '/private/tmp/conversations/run-1',
      messages: [{ id: 'answer', role: 'assistant', text: 'output', status: 'complete' }],
    })
    const list = updateRunList(updateRunList([], run), { ...run, status: 'completed' })
    expect(list).toHaveLength(1)
    expect(list[0].status).toBe('completed')
    expect(list[0].execution_directory).toBe(run.execution_directory)
    expect(list[0].workspace_path).toBeUndefined()
    expect(list[0]).not.toHaveProperty('messages')
    expect(list[0]).not.toHaveProperty('context')
  })

  it('preserves a newly created run and newer event state across a stale list fetch', () => {
    const first = snapshot({ revision: 3, updated_at: '2026-10-21T12:00:10Z', status: 'completed' })
    const second = snapshot({ id: 'run-2', updated_at: '2026-10-21T12:00:20Z' })
    const merged = mergeRunLists([first, second], [snapshot()])
    expect(merged.map((run) => run.id)).toEqual(['run-2', 'run-1'])
    expect(merged[1].status).toBe('completed')
  })

  it('accepts newer status with an identical timestamp and rejects reverse HTTP ordering', () => {
    const running = snapshot({ revision: 7, status: 'running' })
    const completed = snapshot({ revision: 8, status: 'completed' })
    const list = mergeRunLists([running], [completed])
    expect(list[0].status).toBe('completed')
    expect(list[0].revision).toBe(8)
    expect(mergeRunLists(list, [running])[0]).toBe(completed)
    expect(mergeRunLists(list, [{ ...completed, status: 'running' }])[0]).toBe(completed)
  })

  it('preserves manual compaction activity in list updates and clears it on a newer snapshot', () => {
    const idle = snapshot({ revision: 2, status: 'waiting_for_input', compacting: false })
    const compacting = snapshot({ revision: 3, status: 'waiting_for_input', compacting: true })
    const active = updateRunList([idle], compacting)
    expect(active[0].compacting).toBe(true)
    expect(active[0]).not.toHaveProperty('messages')
    expect(mergeRunLists(active, [idle])[0].compacting).toBe(true)
    const finished = { ...idle, revision: 4 }
    expect(mergeRunLists(active, [finished])[0].compacting).toBe(false)
  })
})
