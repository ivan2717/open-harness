import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Bot, ChevronRight, Square, X } from 'lucide-react'
import { useEffect, useRef } from 'react'
import type { RunSummary } from '../../shared/contracts'
import { configOptions, storeSnapshot, useRun } from '../lib/queries'
import { errorMessage } from '../lib/format'
import { Conversation } from './Conversation'
import { InlineError, Loading } from './Feedback'
import { RunActivity } from './RunActivity'
import './agent-panel.css'

const statuses: Record<RunSummary['status'], string> = {
  running: '运行中',
  completed: '已完成',
  waiting_for_input: '等待输入',
  paused: '已暂停',
  interrupted: '已中断',
  cancelled: '已停止',
  failed: '失败',
}

function AgentConversation({ task }: { task: RunSummary }) {
  const query = useRun(task.id)
  const config = useQuery(configOptions)
  const client = useQueryClient()
  const stop = useMutation({
    mutationFn: () => window.harness.cancelRun(task.id),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const run = query.data
  if (query.isPending) return <Loading label="正在读取子智能体消息" />
  if (!run)
    return (
      <InlineError
        message={`无法读取子智能体消息。${errorMessage(query.error)}`}
        retry={() => void query.refetch()}
      />
    )
  const model = config.data?.models.find((entry) => entry.id === run.model_id)
  return (
    <>
      <div className="agent-panel-meta">
        <RunActivity active={run.status === 'running' || Boolean(run.compacting)} />
        <span>{run.compacting ? '整理上下文' : statuses[run.status]}</span>
        <span className="agent-panel-model" title={model?.name ?? run.model_id}>
          {model?.name ?? run.model_id}
        </span>
      </div>
      {(query.streamError || query.isError) && (
        <InlineError message="子智能体消息连接中断，正在尝试恢复。" retry={query.reconnect} />
      )}
      <Conversation run={run} />
      {run.error && <InlineError message={run.error} />}
      {stop.isError && <InlineError message={errorMessage(stop.error)} />}
      {run.status === 'running' && (
        <div className="agent-panel-footer">
          <button
            type="button"
            className="resume-button"
            disabled={stop.isPending}
            onClick={() => stop.mutate()}
          >
            <Square size={12} />
            {stop.isPending ? '正在停止…' : '停止子任务'}
          </button>
        </div>
      )}
    </>
  )
}

export function AgentPanel({
  tasks,
  selectedId,
  onSelect,
  onBack,
  onClose,
}: {
  tasks: RunSummary[]
  selectedId: string | null
  onSelect: (id: string) => void
  onBack: () => void
  onClose: () => void
}) {
  const task = tasks.find((entry) => entry.id === selectedId)
  const close = useRef<HTMLButtonElement>(null)
  // Focus the panel without taking focus away again on live snapshot updates.
  useEffect(() => {
    const previous = document.activeElement
    close.current?.focus({ preventScroll: true })
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus({ preventScroll: true })
    }
  }, [])
  return (
    <aside
      className="agent-panel"
      aria-label="子智能体"
      onKeyDown={(event) => {
        if (event.key === 'Escape' && !event.defaultPrevented) {
          event.stopPropagation()
          onClose()
        }
      }}
    >
      <header className="agent-panel-header">
        <Bot size={16} aria-hidden="true" />
        <span>子智能体</span>
        <button
          ref={close}
          className="icon-button"
          aria-label="关闭子智能体面板"
          title="关闭"
          onClick={onClose}
        >
          <X size={16} />
        </button>
      </header>
      {task ? (
        <>
          <div className="agent-panel-heading">
            <button
              className="icon-button"
              aria-label="返回子智能体列表"
              title="所有子智能体"
              onClick={onBack}
            >
              <ArrowLeft size={16} />
            </button>
            <h2 title={task.delegation?.task_name || task.title}>
              {task.delegation?.task_name || task.title}
            </h2>
          </div>
          <AgentConversation key={task.id} task={task} />
        </>
      ) : (
        <div className="agent-panel-list">
          {selectedId && <p>这个子任务暂时不可用，可选择其他任务查看。</p>}
          {tasks.map((entry) => (
            <button key={entry.id} onClick={() => onSelect(entry.id)}>
              <RunActivity active={entry.status === 'running' || Boolean(entry.compacting)} />
              <span className="agent-panel-list-name">
                {entry.delegation?.task_name || entry.title}
                <small>{statuses[entry.status]}</small>
              </span>
              <ChevronRight size={14} aria-hidden="true" />
            </button>
          ))}
        </div>
      )}
    </aside>
  )
}
