import { useEffect, useRef, useState } from 'react'
import {
  Activity,
  ArrowDown,
  BookOpen,
  Check,
  ChevronRight,
  CircleAlert,
  LoaderCircle,
  Terminal,
  UserRound,
} from 'lucide-react'
import type { ChatMessage, RunDetail } from '../../shared/contracts'
import { toolOutcome } from '../lib/format'
import { skillActivityLabel } from '../lib/skill-activity'
import { FileReferences } from './FileReferences'
import { MessageMarkdown } from './MessageMarkdown'

function Message({ message, runId }: { message: ChatMessage; runId: string }) {
  if (
    message.role === 'assistant' &&
    message.status === 'complete' &&
    !message.text &&
    !message.reasoning
  )
    return null
  const toolRunning = message.status === 'streaming' || message.status === 'pending'
  const outcome = toolOutcome(message)
  const skillActivity = skillActivityLabel(message)
  if (message.role === 'tool')
    return (
      <div className="tool-row">
        <details className={`tool-card ${outcome.failed ? 'tool-error' : ''}`}>
          <summary>
            <span className="tool-card-icon">
              {skillActivity ? <BookOpen size={14} /> : <Terminal size={14} />}
            </span>
            <span className="min-w-0 flex-1 truncate">
              {skillActivity ?? message.tool_name ?? '工具执行'}
            </span>
            <span className="tool-status">
              {toolRunning ? (
                <LoaderCircle className="animate-spin" size={12} />
              ) : outcome.failed ? (
                <CircleAlert size={12} />
              ) : (
                <Check size={12} />
              )}
              {outcome.label}
            </span>
            <ChevronRight className="details-chevron" size={13} />
          </summary>
          <pre>{message.text || '暂无输出'}</pre>
        </details>
      </div>
    )
  return (
    <article className={`message message-${message.role}`}>
      <div className={`message-avatar ${message.role === 'assistant' ? 'assistant-avatar' : ''}`}>
        {message.role === 'user' ? <UserRound size={15} /> : <Activity size={16} />}
      </div>
      <div className="message-body">
        <div className="message-author">
          {message.role === 'user' ? '你' : 'Open Harness'}
          {message.status === 'streaming' && (
            <span className="message-live">
              <span />
              正在生成
            </span>
          )}
        </div>
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
                settled={message.status === 'complete'}
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

export function Conversation({ run }: { run: RunDetail }) {
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
          {run.messages.map((message) => (
            <Message key={message.id} message={message} runId={run.id} />
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
