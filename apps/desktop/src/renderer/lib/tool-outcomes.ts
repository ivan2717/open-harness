import type { ChatMessage } from '../../shared/contracts'
import { toolOutcome } from './format'

export type ToolOutcome = ReturnType<typeof toolOutcome>

const activeStatuses = new Set(['running', 'awaiting_approval'])
const commandStatuses = new Set([
  ...activeStatuses,
  'completed',
  'failed',
  'denied',
  'timed_out',
  'cancelled',
])

function record(value: string): Record<string, unknown> | undefined {
  try {
    const parsed: unknown = JSON.parse(value)
    return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : undefined
  } catch {
    return undefined
  }
}

function commandObservation(message: ChatMessage): { id: string; active: boolean } | undefined {
  if (
    message.role !== 'tool' ||
    !['run_command', 'manage_command'].includes(message.tool_name ?? '') ||
    message.status !== 'complete' ||
    message.is_error
  )
    return undefined
  const result = record(message.text)
  const id = result?.command_id
  const status = result?.status
  if (
    typeof id !== 'string' ||
    !id.trim() ||
    typeof status !== 'string' ||
    !commandStatuses.has(status)
  )
    return undefined
  if (message.tool_name === 'manage_command' && message.tool_arguments) {
    const args = record(message.tool_arguments)
    // A mismatched or malformed request is not evidence about this command.
    if (!args || args.command_id !== id) return undefined
  }
  return { id, active: activeStatuses.has(status) }
}

/**
 * A command launch's response is immutable. Later observations can update its
 * displayed process outcome without replacing output, concealing failed calls
 * or merging unrelated commands. These snapshots never make a completed call
 * pending again; only the call lifecycle controls its activity indicator.
 */
export function resolveToolOutcomes(messages: readonly ChatMessage[]): Map<string, ToolOutcome> {
  const outcomes = new Map<string, ToolOutcome>()
  const unresolved = new Map<string, { ids: string[]; outcome: ToolOutcome }>()
  for (const message of messages) {
    const outcome = toolOutcome(message)
    outcomes.set(message.id, outcome)
    const observation = commandObservation(message)
    if (!observation) continue
    const previous = unresolved.get(observation.id)
    if (message.tool_name === 'manage_command' && previous) {
      previous.outcome = outcome
      if (!observation.active) {
        for (const id of previous.ids) outcomes.set(id, outcome)
        unresolved.delete(observation.id)
      }
    }
    if (observation.active) {
      const group = previous ?? { ids: [], outcome }
      group.ids.push(message.id)
      unresolved.set(observation.id, group)
    }
  }
  for (const group of unresolved.values())
    for (const id of group.ids) outcomes.set(id, group.outcome)
  return outcomes
}
