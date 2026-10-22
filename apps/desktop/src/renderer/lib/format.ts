import type { ChatMessage, RunStatus } from '../../shared/contracts'

export const statusLabels: Record<RunStatus, string> = {
  running: '运行中',
  waiting_for_input: '等待回复',
  completed: '已完成',
  failed: '运行失败',
  cancelled: '已停止',
  interrupted: '已中断',
}

export function number(value: number): string {
  return new Intl.NumberFormat('en-US').format(value)
}

export function compactNumber(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0)}M`
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 ? 1 : 0)}k`
  return String(value)
}

export function time(value: string): string {
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? '—'
    : date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
}

export function dateTime(value: string): string {
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? '—'
    : date.toLocaleString('zh-CN', {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
      })
}

export function errorMessage(error: unknown): string {
  return error instanceof Error
    ? error.message
    : typeof error === 'string'
      ? error
      : '请求未能完成，请重试。'
}

export function toolOutcome(message: ChatMessage): { label: string; failed: boolean } {
  if (message.status === 'pending' || message.status === 'streaming')
    return { label: '执行中', failed: false }
  if (message.is_error) return { label: '失败', failed: true }
  if (message.tool_name === 'run_command') {
    try {
      const result: unknown = JSON.parse(message.text)
      if (result && typeof result === 'object' && !Array.isArray(result)) {
        const { status, exit_code } = result as { status?: unknown; exit_code?: unknown }
        if (status === 'timed_out') return { label: '已超时', failed: true }
        if (status === 'cancelled') return { label: '已停止', failed: true }
        if (status === 'failed' || (typeof exit_code === 'number' && exit_code !== 0))
          return { label: '执行失败', failed: true }
      }
    } catch {
      // Legacy tool output may be plain text instead of a structured command result.
    }
  }
  return { label: '已完成', failed: false }
}
