import type { ChatMessage, RunDetail } from '../../shared/contracts'
import { toolActivity } from './tool-activity'

export interface ConversationTurn {
  id: string
  user?: ChatMessage
  messages: ChatMessage[]
  active: boolean
  startedAt?: string
  completedAt?: string
}

const terminalEvents = new Set([
  'input_requested',
  'run_completed',
  'run_failed',
  'run_cancelled',
  'run_paused',
])

function eventSequence(message?: ChatMessage): number | undefined {
  const match = message?.id.match(/^event-(\d+)$/)
  return match ? Number(match[1]) : undefined
}

/** User inputs delimit visible work; idle time between inputs is never work time. */
export function conversationTurns(run: RunDetail): ConversationTurn[] {
  const turns: ConversationTurn[] = []
  for (const message of run.messages) {
    if (message.role === 'user' || !turns.length) {
      turns.push({
        id: message.id,
        user: message.role === 'user' ? message : undefined,
        messages: [],
        active: false,
      })
    }
    if (message.role !== 'user') turns[turns.length - 1].messages.push(message)
  }
  return turns.map((turn, index) => {
    const sequence = eventSequence(turn.user)
    const nextSequence = eventSequence(turns[index + 1]?.user)
    const events =
      sequence === undefined
        ? []
        : run.events.filter(
            (event) =>
              event.sequence >= sequence &&
              (nextSequence === undefined || event.sequence < nextSequence),
          )
    const terminal = events.find((event) => terminalEvents.has(event.type))
    const active = index === turns.length - 1 && run.status === 'running'
    // An input with no follow-up text (Continue) is still a boundary in the log.
    // Do not use the session update time: a settings change is not task activity.
    const lastMessage = turn.messages.at(-1)
    return {
      ...turn,
      active,
      startedAt: events[0]?.timestamp ?? turn.user?.started_at ?? turn.messages[0]?.started_at,
      completedAt: active
        ? undefined
        : (terminal?.timestamp ?? events.at(-1)?.timestamp ?? lastMessage?.completed_at),
    }
  })
}

export function elapsedLabel(start?: string, end?: string): string | undefined {
  if (!start || !end) return undefined
  const delta = Date.parse(end) - Date.parse(start)
  if (!Number.isFinite(delta) || delta < 0) return undefined
  const seconds = Math.floor(delta / 1000)
  if (seconds < 60) return `${seconds} 秒`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`
}

/** Only adjacent tool messages are grouped. Prose always remains in event order. */
export function activityBlocks(messages: ChatMessage[]): (ChatMessage | ChatMessage[])[] {
  const blocks: (ChatMessage | ChatMessage[])[] = []
  for (const message of messages) {
    if (message.role !== 'tool') {
      if (message.text || message.reasoning || message.status === 'streaming') blocks.push(message)
      continue
    }
    const previous = blocks.at(-1)
    if (
      Array.isArray(previous) &&
      toolActivity(message).kind !== 'agent' &&
      toolActivity(previous[0]).kind !== 'agent'
    )
      previous.push(message)
    else blocks.push([message])
  }
  return blocks
}
