import { describe, expect, it } from 'vitest'
import type { ChatMessage, RunDetail } from '../../shared/contracts'
import { activityBlocks, conversationTurns, elapsedLabel } from './conversation-turns'

const message = (id: string, role: ChatMessage['role'], tool_name?: string): ChatMessage => ({
  id,
  role,
  tool_name,
  text: id,
  status: 'complete',
})
function run(): RunDetail {
  return {
    id: 'run',
    revision: 1,
    title: 'test',
    prompt: 'test',
    model_id: 'model',
    status: 'waiting_for_input',
    created_at: '2026-11-01T10:00:00Z',
    updated_at: '2026-11-02T11:00:00Z',
    context: null,
    messages: [
      message('event-0', 'user'),
      message('tool-a', 'tool', 'read_file'),
      message('event-3', 'assistant'),
      message('event-5', 'user'),
      message('event-7', 'assistant'),
    ],
    events: [
      { sequence: 0, type: 'run_started', timestamp: '2026-11-01T10:00:00Z' },
      { sequence: 3, type: 'model_response_received', timestamp: '2026-11-01T10:02:16Z' },
      { sequence: 4, type: 'input_requested', timestamp: '2026-11-01T10:02:17Z' },
      { sequence: 5, type: 'input_received', timestamp: '2026-11-02T10:00:00Z' },
      { sequence: 7, type: 'model_response_received', timestamp: '2026-11-02T10:00:09Z' },
      { sequence: 8, type: 'input_requested', timestamp: '2026-11-02T10:00:10Z' },
    ].map((event) => ({ ...event, step: 1, label: '', detail: '' })),
  }
}

describe('conversation activity projection', () => {
  it('keeps turns distinct and excludes user idle time and settings updates from elapsed time', () => {
    const turns = conversationTurns(run())
    expect(turns).toHaveLength(2)
    expect(turns[0].messages.map((item) => item.id)).toEqual(['tool-a', 'event-3'])
    expect(turns[1].messages.map((item) => item.id)).toEqual(['event-7'])
    expect(elapsedLabel(turns[0].startedAt, turns[0].completedAt)).toBe('2 分 17 秒')
    expect(elapsedLabel(turns[1].startedAt, turns[1].completedAt)).toBe('10 秒')
    expect(conversationTurns({ ...run(), status: 'running' }).map((turn) => turn.active)).toEqual([
      false,
      true,
    ])
  })
  it('never groups across commentary or collaboration events, and retains order', () => {
    const items = [
      message('a', 'tool', 'read_file'),
      message('b', 'tool', 'run_command'),
      message('c', 'assistant'),
      message('d', 'tool', 'read_file'),
      message('e', 'tool', 'spawn_agent'),
      message('f', 'tool', 'read_file'),
    ]
    expect(
      activityBlocks(items).map((block) =>
        Array.isArray(block) ? block.map((item) => item.id) : block.id,
      ),
    ).toEqual([['a', 'b'], 'c', ['d'], ['e'], ['f']])
  })
  it('supports old records without dates and never invents a duration', () => {
    const turns = conversationTurns({
      ...run(),
      events: [],
      messages: [message('old', 'assistant')],
    })
    expect(turns[0].messages[0].id).toBe('old')
    expect(elapsedLabel(turns[0].startedAt, turns[0].completedAt)).toBeUndefined()
    expect(elapsedLabel('invalid', 'invalid')).toBeUndefined()
    expect(elapsedLabel('2026-11-02', '2026-11-01')).toBeUndefined()
  })
})
