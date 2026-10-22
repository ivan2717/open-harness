import { describe, expect, it } from 'vitest'
import type { ChatMessage } from '../../shared/contracts'
import { toolOutcome } from './format'

describe('command results in the conversation', () => {
  it('distinguishes a returned command failure from a successfully executed command', () => {
    const message: ChatMessage = {
      id: 'command',
      role: 'tool',
      tool_name: 'run_command',
      status: 'complete',
      text: '',
    }
    const outcome = (result: unknown) => toolOutcome({ ...message, text: JSON.stringify(result) })
    expect(outcome({ status: 'completed', exit_code: 0 })).toEqual({
      label: '已完成',
      failed: false,
    })
    expect(outcome({ status: 'failed', exit_code: null })).toEqual({
      label: '执行失败',
      failed: true,
    })
    expect(outcome({ exit_code: 2 })).toEqual({ label: '执行失败', failed: true })
    expect(outcome({ status: 'timed_out', exit_code: null })).toEqual({
      label: '已超时',
      failed: true,
    })
    expect(toolOutcome({ ...message, status: 'pending' })).toEqual({
      label: '执行中',
      failed: false,
    })
    expect(toolOutcome({ ...message, text: 'Legacy text' })).toEqual({
      label: '已完成',
      failed: false,
    })
    expect(toolOutcome({ ...message, is_error: true })).toEqual({ label: '失败', failed: true })
  })
})
