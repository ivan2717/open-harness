import { useEffect, useRef, useState } from 'react'
import { ArrowDown, ChevronRight, LoaderCircle } from 'lucide-react'
import type { ChatMessage, RunDetail } from '../../shared/contracts'
import { resolveToolOutcomes, type ToolOutcome } from '../lib/tool-outcomes'
import {
  activityBlocks,
  conversationTurns,
  elapsedLabel,
  type ConversationTurn,
} from '../lib/conversation-turns'
import { FileReferences } from './FileReferences'
import { MessageMarkdown } from './MessageMarkdown'
import { ToolActivityGroup } from './ToolActivity'
import '../conversation-activity.css'

function Message({ message, runId }: { message: ChatMessage; runId: string }) {
  if (
    message.role === 'assistant' &&
    message.status === 'complete' &&
    !message.text &&
    !message.reasoning
  )
    return null
  return (
    <article
      className={`message message-${message.role}`}
      aria-label={message.role === 'user' ? '你的消息' : undefined}
    >
      <div className="message-body">
        {message.status === 'interrupted' && (
          <div className="message-author">
            <span className="message-interrupted">已中断</span>
          </div>
        )}
        {message.files && (
          <FileReferences files={message.files} runId={runId} messageId={message.id} />
        )}
        {message.reasoning && (
          <details className="reasoning-block">
            <summary>
              <span className="reasoning-mark" />
              模型思考<span className="reasoning-description">公开返回的内容</span>
              <ChevronRight size={12} className="details-chevron" />
            </summary>
            <p>{message.reasoning}</p>
          </details>
        )}
        {message.text ? (
          <div className={message.role === 'assistant' ? 'message-rich-text' : 'message-text'}>
            {message.role === 'assistant' ? (
              <MessageMarkdown
                text={message.text}
                runId={runId}
                messageId={message.id}
                settled={message.status !== 'streaming'}
              />
            ) : (
              message.text
            )}
            {message.status === 'streaming' && (
              <span className="stream-cursor" aria-hidden="true" />
            )}
          </div>
        ) : message.status === 'streaming' ? (
          <div className="thinking-dots" aria-label="正在生成">
            <i />
            <i />
            <i />
          </div>
        ) : null}
      </div>
    </article>
  )
}

function AssistantTurn({
  turn,
  runId,
  resolved,
}: {
  turn: ConversationTurn
  runId: string
  resolved: Map<string, ToolOutcome>
}) {
  const [choice, setChoice] = useState<boolean | null>(null)
  const [now, setNow] = useState(() => new Date().toISOString())
  useEffect(() => {
    if (!turn.active) return
    setNow(new Date().toISOString())
    const timer = window.setInterval(() => setNow(new Date().toISOString()), 1_000)
    return () => window.clearInterval(timer)
  }, [turn.active])
  const visible = turn.messages.filter(
    (message) =>
      message.role === 'tool' ||
      message.text ||
      message.reasoning ||
      message.status === 'streaming',
  )
  const tail = visible.at(-1)
  // The runtime supports different model protocols, not all of which provide a
  // final/commentary phase. Only a settled trailing answer is placed outside work.
  const answer = !turn.active && tail?.role === 'assistant' ? tail : undefined
  const work = answer ? visible.slice(0, -1) : visible
  const outcomes = new Map(resolved)
  if (!turn.active)
    for (const message of turn.messages) {
      if (
        message.role === 'tool' &&
        (message.status === 'pending' || message.status === 'streaming')
      )
        outcomes.set(message.id, { label: '已中断', failed: true })
    }
  const hasOperations = work.some((message) => message.role === 'tool')
  const failures = work.filter(
    (message) => message.role === 'tool' && outcomes.get(message.id)?.failed,
  ).length
  const duration = elapsedLabel(turn.startedAt, turn.completedAt ?? (turn.active ? now : undefined))
  const open = choice ?? turn.active
  const content = activityBlocks(work).map((block) =>
    Array.isArray(block) ? (
      <ToolActivityGroup key={block[0].id} messages={block} runId={runId} resolved={outcomes} />
    ) : (
      <Message key={block.id} message={block} runId={runId} />
    ),
  )
  return (
    <section className="assistant-turn" aria-label="助手回复">
      {hasOperations ? (
        <details className="turn-work" open={open}>
          <summary
            onClick={(event) => {
              event.preventDefault()
              setChoice(!open)
            }}
          >
            {turn.active && <LoaderCircle size={13} className="animate-spin" />}
            <span>
              {turn.active ? '正在处理' : '处理过程'}
              {duration ? ` · 用时 ${duration}` : ''}
            </span>
            {failures > 0 && (
              <span className="activity-status activity-failed">{failures} 项失败</span>
            )}
            <ChevronRight size={13} className="details-chevron" />
          </summary>
          <div className="turn-work-content">{content}</div>
        </details>
      ) : (
        content
      )}
      {answer && (
        <div className="turn-answer">
          <Message message={answer} runId={runId} />
        </div>
      )}
    </section>
  )
}

export function Conversation({ run }: { run: RunDetail }) {
  const turns = conversationTurns(run)
  const outcomes = resolveToolOutcomes(run.messages)
  const scroll = useRef<HTMLDivElement>(null)
  const keepAtBottom = useRef(true)
  const [showScroll, setShowScroll] = useState(false)
  const previousRun = useRef(run.id)
  useEffect(() => {
    if (previousRun.current !== run.id) {
      keepAtBottom.current = true
      previousRun.current = run.id
    }
    if (keepAtBottom.current && scroll.current)
      scroll.current.scrollTop = scroll.current.scrollHeight
  }, [run.id, run.revision])
  useEffect(() => {
    const element = scroll.current
    const content = element?.firstElementChild
    if (!element || !content) return
    const observer = new ResizeObserver(() => {
      if (keepAtBottom.current) element.scrollTop = element.scrollHeight
    })
    observer.observe(content)
    return () => observer.disconnect()
  }, [run.id])

  return (
    <div className="conversation-container">
      <div
        ref={scroll}
        className="conversation-scroll"
        onScroll={() => {
          const element = scroll.current
          if (!element) return
          keepAtBottom.current =
            element.scrollHeight - element.scrollTop - element.clientHeight < 100
          setShowScroll(!keepAtBottom.current)
        }}
      >
        <div className="conversation-content">
          <div className="conversation-start">
            <span />
            {new Date(run.created_at).toLocaleDateString('zh-CN', {
              month: 'long',
              day: 'numeric',
            })}
            <span />
          </div>
          {turns.map((turn) => (
            <div className="conversation-turn" key={`${run.id}:${turn.id}`}>
              {turn.user && <Message message={turn.user} runId={run.id} />}
              <AssistantTurn turn={turn} runId={run.id} resolved={outcomes} />
            </div>
          ))}
          {run.status === 'running' &&
            !run.messages.some((message) => message.status === 'streaming') && (
              <div className="run-progress">
                <LoaderCircle size={14} className="animate-spin" />
                <span>Harness 正在执行下一步…</span>
              </div>
            )}
        </div>
      </div>
      {showScroll && (
        <button
          className="scroll-bottom"
          onClick={() => {
            keepAtBottom.current = true
            scroll.current?.scrollTo({ top: scroll.current.scrollHeight, behavior: 'smooth' })
            setShowScroll(false)
          }}
        >
          <ArrowDown size={14} />
          最新消息
        </button>
      )}
    </div>
  )
}
