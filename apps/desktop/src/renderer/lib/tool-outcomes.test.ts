import { describe, expect, it } from 'vitest'
import type { ChatMessage } from '../../shared/contracts'
import { resolveToolOutcomes } from './tool-outcomes'

function command(
  id: string,
  tool_name: 'run_command' | 'manage_command',
  command_id: string,
  status: string,
  extra: Partial<ChatMessage> = {},
): ChatMessage {
  return {
    id,
    role: 'tool',
    tool_name,
    status: 'complete',
    tool_arguments: JSON.stringify(
      tool_name === 'run_command' ? { command: 'node task.mjs' } : { command_id, action: 'output' },
    ),
    text: JSON.stringify({
      command_id,
      status,
      exit_code: status === 'completed' ? 0 : null,
      output: 'original output',
      next_cursor: 15,
      has_more_output: false,
      output_truncated: false,
    }),
    ...extra,
  }
}

const launch = (id: string, status = 'running') =>
  command(`launch-${id}`, 'run_command', id, status)
const poll = (id: string, status = 'completed') =>
  command(`poll-${id}`, 'manage_command', id, status)

describe('resolved command activity outcomes', () => {
  it('resolves recorded process outcomes using later output for the same command', () => {
    const started = launch('one', 'awaiting_approval')
    const running = command('poll-running', 'manage_command', 'one', 'running')
    const finished = poll('one')
    const before = [started, running, finished].map((message) => message.text)
    const result = resolveToolOutcomes([started, running, finished])
    for (const message of [started, running, finished])
      expect(result.get(message.id)).toEqual({ label: '已完成', failed: false })
    expect([started, running, finished].map((message) => message.text)).toEqual(before)
  })

  it('reflects approval progress without inventing a terminal result', () => {
    const result = resolveToolOutcomes([launch('one', 'awaiting_approval'), poll('one', 'running')])
    expect(result.get('launch-one')).toEqual({ label: '返回时仍在运行', failed: false })
  })

  it.each([
    ['failed', '执行失败'],
    ['denied', '已拒绝'],
    ['timed_out', '已超时'],
    ['cancelled', '已停止'],
  ])('resolves earlier active records to confirmed %s status', (status, label) => {
    expect(resolveToolOutcomes([launch('one'), poll('one', status)]).get('launch-one')).toEqual({
      label,
      failed: true,
    })
  })

  it('never merges different commands, unknown tools or ordinary assistant text', () => {
    const otherTool = { ...poll('one'), id: 'other-tool', tool_name: 'call_mcp_tool' }
    const assistant = { ...poll('one'), id: 'assistant', role: 'assistant' as const }
    const result = resolveToolOutcomes([launch('one'), poll('two'), otherTool, assistant])
    expect(result.get('launch-one')).toEqual({ label: '返回时仍在运行', failed: false })
    expect(result.get('poll-two')).toEqual({ label: '已完成', failed: false })
    expect(result.size).toBe(4)
  })

  it('does not overwrite failed launches, failed polling calls or previous terminal observations', () => {
    const failedLaunch = launch('one', 'failed')
    const toolFailure = { ...poll('two'), id: 'failed-read', is_error: true }
    const result = resolveToolOutcomes([
      failedLaunch,
      launch('two'),
      toolFailure,
      poll('one'),
      poll('two'),
    ])
    expect(result.get('launch-one')).toEqual({ label: '执行失败', failed: true })
    expect(result.get('failed-read')).toEqual({ label: '失败', failed: true })
    expect(result.get('launch-two')).toEqual({ label: '已完成', failed: false })
    const terminal = resolveToolOutcomes([
      launch('three'),
      poll('three', 'failed'),
      { ...poll('three'), id: 'later' },
    ])
    expect(terminal.get('launch-three')).toEqual({ label: '执行失败', failed: true })
  })

  it('ignores unresolved requests, malformed results, missing ids and mismatched request ids', () => {
    for (const extra of [
      { status: 'pending' as const },
      { status: 'streaming' as const },
      { is_error: true },
      { text: 'not json' },
      { text: '{"status":"completed"}' },
      { text: '{"command_id":"one","status":"unknown"}' },
      { tool_arguments: '{"command_id":"other"}' },
      { tool_arguments: 'invalid' },
    ]) {
      const result = resolveToolOutcomes([launch('one'), { ...poll('one'), ...extra }])
      expect(result.get('launch-one')).toEqual({ label: '返回时仍在运行', failed: false })
    }
  })

  it('uses legacy observations with a confirmed command id but no saved request arguments', () => {
    const result = resolveToolOutcomes([
      launch('one'),
      { ...poll('one'), tool_arguments: undefined },
    ])
    expect(result.get('launch-one')).toEqual({ label: '已完成', failed: false })
  })

  it('does not apply earlier observations to a later launch', () => {
    const result = resolveToolOutcomes([poll('one'), launch('one')])
    expect(result.get('launch-one')).toEqual({ label: '返回时仍在运行', failed: false })
  })
})
