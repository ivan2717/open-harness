import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BookOpen, ChevronRight, LoaderCircle, RefreshCw } from 'lucide-react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import type { RunDetail } from '../../shared/contracts'
import type { MemoryTask, RunMemory } from '../../shared/memory'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import '../memory.css'

export function MemoryInspector({
  run,
  onSource,
}: {
  run?: RunDetail
  onSource: (sequence: number) => void
}) {
  const query = useQuery({
    queryKey: ['run-memory', run?.id],
    queryFn: () => window.harness.runMemory(run!.id),
    enabled: Boolean(run),
    // This read-only poll runs only while this tab is mounted. Compilation can
    // finish after the run becomes idle, so keep observing its saved revision.
    refetchInterval: run?.status === 'running' || run?.compacting ? 1_500 : 5_000,
    retry: 1,
  })
  if (!run)
    return (
      <div className="inspector-empty timeline-empty">
        <BookOpen size={25} strokeWidth={1.3} />
        <p>会话记忆</p>
        <small>开始会话后，在这里查看决策原因、失败经验和结果依据。</small>
      </div>
    )
  return (
    <div className="memory-inspector">
      <div className="memory-heading">
        <span className="section-eyebrow">
          <BookOpen size={14} />
          会话 Wiki
        </span>
        <button
          type="button"
          className="icon-button"
          aria-label="刷新会话记忆"
          title="刷新会话记忆"
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          <RefreshCw size={13} className={query.isFetching ? 'animate-spin' : ''} />
        </button>
      </div>
      {query.isPending && (
        <p className="memory-note" role="status">
          <LoaderCircle size={13} className="animate-spin" /> 正在读取记忆…
        </p>
      )}
      {query.error && (
        <InlineError message={errorMessage(query.error)} retry={() => void query.refetch()} />
      )}
      {query.data && <MemorySnapshot memory={query.data} onSource={onSource} />}
      <Link to="/extensions" search={{ tab: 'plugins' }} className="memory-settings-link">
        管理记忆插件 <ChevronRight size={12} />
      </Link>
    </div>
  )
}

export function MemorySnapshot({
  memory,
  onSource,
}: {
  memory: RunMemory
  onSource: (sequence: number) => void
}) {
  const hasContent = Boolean(memory.overview || memory.pages.length || memory.tasks?.length)
  return (
    <>
      <p className="memory-note" role="status">
        {!memory.enabled || memory.status === 'disabled'
          ? 'Wiki 已关闭 · 停止自动整理和 Wiki 读取，已保存的记忆仍可查看。上下文压缩和原始会话回查不受影响。'
          : memory.status === 'error'
            ? '整理暂未完成，已保存的记忆保持不变。'
            : hasContent
              ? '保存当前会话的历史决策、经验和依据，按需提供相关内容。'
              : '尚无记忆。积累新进展后自动整理，压缩前会检查记忆覆盖情况。'}
      </p>
      {memory.error && <InlineError message={memory.error} />}
      {hasContent && (
        <>
          <p className="memory-revision">
            版本 {memory.revision}
            {memoryCoverage(memory)}
          </p>
          {memory.enabled && memory.pending && (
            <p className="memory-note">还有内容待整理，记忆可能暂时落后于会话。</p>
          )}
          {Boolean(memory.tasks?.length) && (
            <section className="memory-overview" aria-label="任务记录">
              <h3>任务记录</h3>
              {memory.tasks!.map((task) => (
                <TaskRecord key={task.id} task={task} onSource={onSource} />
              ))}
            </section>
          )}
          {memory.overview && (
            <section className="memory-overview" aria-label="记忆概览">
              <h3>概览</h3>
              <MemoryText text={memory.overview} />
            </section>
          )}
          <div className="memory-pages">
            {memory.pages.map((page) => (
              <details className="memory-page" key={page.id}>
                <summary>
                  <ChevronRight size={13} />
                  {page.title}
                </summary>
                <MemoryText text={page.content} />
                {page.source_sequences.length > 0 && (
                  <div className="memory-sources" aria-label={`${page.title}的来源事件`}>
                    <span>来源</span>
                    {page.source_sequences.map((sequence) => (
                      <button
                        key={sequence}
                        type="button"
                        title={`在时间线中查看事件 #${sequence}`}
                        onClick={() => onSource(sequence)}
                      >
                        #{sequence}
                      </button>
                    ))}
                  </div>
                )}
              </details>
            ))}
          </div>
          <p className="memory-note memory-footnote">
            记忆可能有遗漏或过时。任务状态附有原文依据，重要结论请结合来源和当前实际情况核对。
          </p>
        </>
      )}
    </>
  )
}

function memoryCoverage(memory: RunMemory): string {
  if (memory.source_cursor) {
    const { sequence, offset } = memory.source_cursor
    return offset > 0 ? ` · 事件 #${sequence} 分段整理中` : ` · 已整理至事件 #${sequence - 1}`
  }
  if (memory.source_cursor === null && memory.pending) return ' · 旧版记忆等待完整补读'
  return memory.through_sequence === null ? '' : ` · 已整理至事件 #${memory.through_sequence}`
}

const taskKinds: Record<MemoryTask['kind'], string> = {
  goal: '目标',
  constraint: '约束',
  decision: '决定',
  work: '工作',
  question: '待解决问题',
}
const taskStatuses: Record<MemoryTask['status'], string> = {
  active: '当前有效',
  planned: '计划中',
  attempted: '已尝试',
  completed: '有完成结果',
  failed: '失败',
  superseded: '已被替代',
  uncertain: '待核实',
}

function TaskRecord({
  task,
  onSource,
}: {
  task: MemoryTask
  onSource: (sequence: number) => void
}) {
  return (
    <details className="memory-page">
      <summary>
        <ChevronRight size={13} />
        {taskKinds[task.kind]} · {taskStatuses[task.status]}
      </summary>
      <p className="memory-markdown">{task.text}</p>
      {task.evidence.map((source, index) => (
        <div key={`${source.sequence}-${index}`} className="memory-task-evidence">
          <blockquote>{source.quote}</blockquote>
          <div className="memory-sources">
            <button
              type="button"
              title={`在时间线中查看事件 #${source.sequence}`}
              onClick={() => onSource(source.sequence)}
            >
              来源 #{source.sequence}
            </button>
          </div>
        </div>
      ))}
    </details>
  )
}

function MemoryText({ text }: { text: string }) {
  return (
    <div className="memory-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        // Memory is a derived note, never a source of executable HTML, network
        // requests or filesystem grants. Navigation is limited to event IDs.
        components={{
          a: ({ children }) => <span>{children}</span>,
          img: ({ alt }) => <span>{alt}</span>,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}
