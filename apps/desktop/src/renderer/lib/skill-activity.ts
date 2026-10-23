import type { ChatMessage } from '../../shared/contracts'

/** A chosen skill is only an instruction; activity comes from real tool records. */
export function skillActivityLabel(message: ChatMessage): string | null {
  if (message.role !== 'tool' || message.tool_name !== 'load_skill') return null
  if (message.is_error) return '技能加载失败'
  let result: Record<string, unknown> | undefined
  try {
    const value: unknown = JSON.parse(message.text)
    if (value && typeof value === 'object' && !Array.isArray(value))
      result = value as Record<string, unknown>
  } catch {
    // Older persisted records can contain plain text instead of structured output.
  }
  const displayName = result?.name || result?.id
  const name =
    typeof displayName === 'string' ? displayName.replace(/\s+/g, ' ').trim().slice(0, 160) : ''
  const suffix = name ? ` · ${name}` : ''
  if (message.status !== 'complete') return `加载技能${suffix}`
  if (typeof result?.content !== 'string') return '技能读取已完成'
  if (
    typeof result.next_offset === 'number' ||
    (typeof result.offset === 'number' && result.offset > 0)
  )
    return `已读取技能片段${suffix}`
  return `已加载技能${suffix}`
}
