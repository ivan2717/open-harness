import { useState } from 'react'
import {
  BookOpen,
  ChevronRight,
  CircleAlert,
  FilePenLine,
  FolderSearch,
  Globe,
  LoaderCircle,
  Monitor,
  Puzzle,
  Terminal,
  Users,
} from 'lucide-react'
import type { ChatMessage } from '../../shared/contracts'
import { toolOutcome } from '../lib/format'
import { elapsedLabel } from '../lib/conversation-turns'
import { toolActivity, type ToolActivityKind } from '../lib/tool-activity'
import { resolveAgentActivity } from '../lib/agent-activity'
import { AgentActivity } from './AgentActivity'
import { useAgentNavigation } from './AgentNavigation'
import { LocalMessageResource } from './MessageResource'

const icons = {
  command: Terminal,
  read: BookOpen,
  write: FilePenLine,
  search: FolderSearch,
  skill: BookOpen,
  browser: Globe,
  computer: Monitor,
  agent: Users,
  integration: Puzzle,
  other: Puzzle,
}
const categories: Record<ToolActivityKind, string> = {
  command: '运行命令',
  read: '读取文件',
  write: '修改文件',
  search: '搜索文件',
  skill: '使用技能',
  browser: '浏览网页',
  computer: '操作电脑',
  agent: '协作任务',
  integration: '使用外部工具',
  other: '工具操作',
}

function pretty(value: string): string {
  try {
    return JSON.stringify(JSON.parse(value), null, 2)
  } catch {
    return value
  }
}

/** Keep command output readable without losing the original structured result. */
function outputText(message: ChatMessage): string {
  try {
    const result = JSON.parse(message.text)
    if (message.tool_name === 'run_command' || message.tool_name === 'manage_command') {
      const parts = ['stdout', 'stderr', 'output'].flatMap((key) =>
        typeof result?.[key] === 'string' && result[key] ? [result[key]] : [],
      )
      if (parts.length) return parts.join('\n')
    }
    if (message.tool_name === 'call_mcp_tool' && Array.isArray(result?.content)) {
      const parts = result.content.flatMap((block: { type?: string; text?: unknown }) =>
        block?.type === 'text' && typeof block.text === 'string' ? [block.text] : [],
      )
      if (parts.length) return parts.join('\n\n')
    }
  } catch {
    /* Older tools can return plain text. */
  }
  return pretty(message.text)
}

type Outcome = ReturnType<typeof toolOutcome>

function callIsRunning(message: ChatMessage, outcome: Outcome): boolean {
  // A completed status query can describe a live background process. Animate
  // only an outstanding call, respecting the interrupted-turn override.
  return (message.status === 'pending' || message.status === 'streaming') && !outcome.failed
}

export function ToolActivity({
  message,
  runId,
  outcome = toolOutcome(message),
}: {
  message: ChatMessage
  runId: string
  outcome?: Outcome
}) {
  const navigation = useAgentNavigation()
  const agents =
    navigation && !outcome.failed ? resolveAgentActivity(message, navigation.runs, runId) : null
  if (navigation && agents) {
    return (
      <AgentActivity
        targets={agents.targets}
        label={toolActivity(message).label}
        selectedId={navigation.selectedId}
        openAgent={navigation.openAgent}
      >
        <RawToolActivity message={message} runId={runId} outcome={outcome} secondary />
      </AgentActivity>
    )
  }
  return <RawToolActivity message={message} runId={runId} outcome={outcome} />
}

function RawToolActivity({
  message,
  runId,
  outcome,
  secondary = false,
}: {
  message: ChatMessage
  runId: string
  outcome: Outcome
  secondary?: boolean
}) {
  const [expanded, setExpanded] = useState(false)
  const activity = toolActivity(message)
  const running = callIsRunning(message, outcome)
  const Icon = icons[activity.kind]
  const duration = elapsedLabel(message.started_at, message.completed_at)
  const succeeded = outcome.label === '已完成'
  const returned = message.status === 'complete' && !outcome.failed && outcome.label !== '待批准'
  const title = `${returned ? '已' : ''}${activity.label}`
  const output = expanded ? outputText(message) : ''
  let command: string | undefined
  try {
    const args = JSON.parse(message.tool_arguments ?? '{}')
    if (activity.kind === 'command' && typeof args.command === 'string') command = args.command
  } catch {
    /* Malformed legacy arguments remain available as text below. */
  }
  return (
    <details
      className={`tool-activity${outcome.failed ? ' activity-failed' : ''}${secondary ? ' agent-call-details' : ''}`}
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary title={[title, activity.detail].filter(Boolean).join(' · ')}>
        {!secondary &&
          (running ? <LoaderCircle size={14} className="animate-spin" /> : <Icon size={14} />)}
        <span className="activity-label">{secondary ? '操作详情' : title}</span>
        {!secondary && activity.detail && (
          <span className="activity-target">{activity.detail}</span>
        )}
        {!secondary && !succeeded && (
          <span className="activity-status">
            {outcome.failed && <CircleAlert size={12} />}
            {outcome.label}
          </span>
        )}
        <ChevronRight size={12} className="details-chevron" />
      </summary>
      {expanded && (
        <div className="activity-detail">
          <div className="activity-detail-heading">
            <span>{command ? 'Shell' : activity.operation}</span>
            <span>
              {outcome.label}
              {duration ? ` · ${duration}` : ''}
            </span>
          </div>
          {command && <pre className="activity-command">$ {command}</pre>}
          {message.status !== 'pending' && <pre>{output || '暂无输出'}</pre>}
          {message.status === 'pending' && outcome.label === '已中断' && (
            <pre>执行已中断，结果尚未确认。</pre>
          )}
          {message.status !== 'pending' && output !== pretty(message.text) && (
            <details className="activity-arguments">
              <summary>
                完整结果
                <ChevronRight size={12} className="details-chevron" />
              </summary>
              <pre>{pretty(message.text)}</pre>
            </details>
          )}
          {message.tool_arguments && (
            <details className="activity-arguments">
              <summary>
                调用参数
                <ChevronRight size={12} className="details-chevron" />
              </summary>
              <pre>{pretty(message.tool_arguments)}</pre>
            </details>
          )}
          {message.images?.map((_, index) => (
            <LocalMessageResource
              key={index}
              reference={{ runId, messageId: message.id, imageIndex: index }}
              label={`操作截图 ${index + 1}`}
            />
          ))}
        </div>
      )}
    </details>
  )
}

export function ToolActivityGroup({
  messages,
  runId,
  resolved,
}: {
  messages: ChatMessage[]
  runId: string
  resolved: Map<string, Outcome>
}) {
  const [choice, setChoice] = useState<boolean | null>(null)
  if (messages.length === 1)
    return (
      <ToolActivity message={messages[0]} runId={runId} outcome={resolved.get(messages[0].id)} />
    )
  const outcomes = messages.map((message) => resolved.get(message.id) ?? toolOutcome(message))
  const running = messages.some((message, index) => callIsRunning(message, outcomes[index]))
  const failed = outcomes.filter((outcome) => outcome.failed).length
  const kinds = [...new Set(messages.map((message) => toolActivity(message).kind))]
  const Icon = icons[kinds[0]]
  const summary = kinds
    .slice(0, 3)
    .map((kind) => categories[kind])
    .join('、')
  const open = choice ?? running
  return (
    <details className="tool-activity-group" open={open}>
      <summary
        onClick={(event) => {
          event.preventDefault()
          setChoice(!open)
        }}
      >
        {running ? <LoaderCircle size={14} className="animate-spin" /> : <Icon size={14} />}
        <span className="activity-group-label">{summary}</span>
        <span className="activity-count">{messages.length} 次操作</span>
        {failed > 0 && <span className="activity-status activity-failed">{failed} 项失败</span>}
        <ChevronRight size={12} className="details-chevron" />
      </summary>
      <div className="activity-group-items">
        {messages.map((message) => (
          <ToolActivity
            key={message.id}
            message={message}
            runId={runId}
            outcome={resolved.get(message.id)}
          />
        ))}
      </div>
    </details>
  )
}
