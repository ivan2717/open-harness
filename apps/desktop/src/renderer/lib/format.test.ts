import { describe, expect, it } from 'vitest'
import type { ChatMessage } from '../../shared/contracts'
import { toolOutcome } from './format'

describe('command results in the conversation', () => {
  it.each(['run_command', 'manage_command'])(
    '%s distinguishes command lifecycle from a successful tool invocation',
    (tool_name) => {
      const message: ChatMessage = {
        id: 'command',
        role: 'tool',
        tool_name,
        status: 'complete',
        text: '',
      }
      const outcome = (result: unknown) => toolOutcome({ ...message, text: JSON.stringify(result) })
      expect(outcome({ status: 'running', exit_code: null })).toEqual({
        label: '执行中',
        failed: false,
      })
      expect(outcome({ status: 'awaiting_approval', exit_code: null })).toEqual({
        label: '待批准',
        failed: false,
      })
      expect(outcome({ status: 'denied', exit_code: null })).toEqual({
        label: '已拒绝',
        failed: true,
      })
      expect(outcome({ status: 'completed', exit_code: 0 })).toEqual({
        label: '已完成',
        failed: false,
      })
      expect(outcome({ status: 'failed', exit_code: null })).toEqual({
        label: '执行失败',
        failed: true,
      })
      expect(outcome({ exit_code: 2 })).toEqual({ label: '执行失败', failed: true })
      expect(outcome({ status: 'failed', exit_code: 125 })).toEqual({
        label: '执行失败',
        failed: true,
      })
      expect(outcome({ status: 'timed_out', exit_code: null })).toEqual({
        label: '已超时',
        failed: true,
      })
      expect(outcome({ status: 'cancelled', exit_code: null })).toEqual({
        label: '已停止',
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
    },
  )
})
