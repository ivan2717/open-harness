import { useEffect, useState, type ReactNode } from 'react'
import {
  ArrowDown,
  BookOpen,
  Check,
  ChevronRight,
  CircleDashed,
  Clock3,
  Layers3,
  Maximize2,
  PanelRightClose,
  ScanLine,
} from 'lucide-react'
import type { ContextSnapshot, RunDetail } from '../../shared/contracts'
import { compactNumber, dateTime, number, time } from '../lib/format'
import { useUiStore } from '../state/ui-store'
import { MemoryInspector } from './MemoryInspector'

function Metric({ label, value, note }: { label: string; value: string; note?: string }) {
  return (
    <div className="context-metric">
      <span>{label}</span>
      <strong>{value}</strong>
      {note && <small>{note}</small>}
    </div>
  )
}

function ContextUsage({
  context,
  compactionAction,
}: {
  context: ContextSnapshot
  compactionAction?: ReactNode
}) {
  const inputTokens = context.accounting?.estimated_input_tokens ?? context.estimated_input_tokens
  const decisionTokens = context.accounting?.decision_input_tokens ?? inputTokens
  const marginTokens = context.accounting?.estimation_margin_tokens ?? 0
  const source = context.accounting?.source ?? context.measurement
  const sourceLabel =
    source === 'provider_calibrated'
      ? context.accounting?.measured_input_tokens == null
        ? '用量修正估算'
        : '提供方校准'
      : source === 'provider_native'
        ? '原生计量 + 估算'
        : source === 'provider_counted'
          ? '历史服务端计数'
          : source === 'tokenizer_estimated'
            ? '历史 tokenizer 估算'
            : source === 'provider'
              ? '提供方计量'
              : '本地估算'
  const ratio = context.input_budget > 0 ? decisionTokens / context.input_budget : 0
  const percent = Math.min(100, Math.max(0, ratio * 100))
  const triggerPosition =
    context.input_budget > 0 ? (context.trigger_tokens / context.input_budget) * 100 : 0
  return (
    <>
      <section className="inspector-section">
        <div className="section-eyebrow">
          <ScanLine size={14} />
          上下文预算
          <span className="measurement-label">{sourceLabel}</span>
        </div>
        <div className="usage-total">
          <strong>{number(decisionTokens)}</strong>
          <span>/ {compactNumber(context.input_budget)} tokens</span>
        </div>
        <div
          className="usage-progress"
          role="progressbar"
          aria-label="上下文输入预算使用比例"
          aria-valuenow={Math.round(percent)}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <span className={ratio >= 0.85 ? 'usage-warning' : ''} style={{ width: `${percent}%` }} />
          {triggerPosition > 0 && triggerPosition < 100 && (
            <i
              style={{ left: `${triggerPosition}%` }}
              title={`压缩触发点 ${number(context.trigger_tokens)}`}
            />
          )}
        </div>
        <div className="usage-caption">
          <span>已用 {percent.toFixed(1)}%</span>
          <span>
            {ratio > 1
              ? '已超出输入预算'
              : `余量 ${compactNumber(Math.max(0, context.input_budget - decisionTokens))}`}
          </span>
        </div>
        <p className="mt-2 text-[11px] leading-relaxed text-muted">
          最近准备的输入 {number(inputTokens)}
          {marginTokens > 0 ? ` + 估算余量 ${number(marginTokens)}` : ''}
          ，用于判断是否压缩。输出预留另计。
        </p>
        {context.observed_usage && (
          <p
            className="mt-2 text-[11px] leading-relaxed text-muted"
            title={`已记录 ${number(context.observed_usage.responses)} 次响应的服务端用量；不含缺失用量或失败请求。`}
          >
            已观测累计：输入 {compactNumber(context.observed_usage.input_tokens)} · 输出{' '}
            {compactNumber(context.observed_usage.output_tokens)}
          </p>
        )}
        <div className="metric-grid">
          <Metric label="模型窗口" value={compactNumber(context.window_tokens)} />
          <Metric label="输出预留" value={compactNumber(context.reserved_output_tokens)} />
          <Metric label="压缩触发点" value={compactNumber(context.trigger_tokens)} />
          <Metric label="压缩目标" value={compactNumber(context.target_tokens)} />
        </div>
        {compactionAction && <div className="mt-4">{compactionAction}</div>}
      </section>
      <section className="inspector-section">
        <div className="section-eyebrow">
          <Layers3 size={14} />
          压缩检查点<span className="count-badge">{context.compaction_count}</span>
        </div>
        {context.checkpoints.length ? (
          <div className="checkpoint-list">
            {context.checkpoints
              .slice()
              .reverse()
              .map((checkpoint, index) => (
                <details key={checkpoint.id} className="checkpoint">
                  <summary>
                    <span className="checkpoint-icon">
                      <Check size={12} />
                    </span>
                    <span className="min-w-0 flex-1">
                      <strong>上下文压缩 {context.checkpoints.length - index}</strong>
                      <small>{dateTime(checkpoint.created_at)}</small>
                    </span>
                    <ChevronRight size={13} className="details-chevron" />
                  </summary>
                  <div className="checkpoint-body">
                    <div className="checkpoint-tokens">
                      <span>{compactNumber(checkpoint.before_tokens)}</span>
                      <ArrowDown size={12} />
                      <strong>{compactNumber(checkpoint.after_tokens)}</strong>
                      <small>tokens</small>
                    </div>
                    <p className="whitespace-pre-wrap break-words">
                      {checkpoint.summary || '检查点已保存，未包含可展示的摘要。'}
                    </p>
                    <code>{checkpoint.id}</code>
                  </div>
                </details>
              ))}
          </div>
        ) : (
          <div className="inspector-empty">
            <CircleDashed size={24} strokeWidth={1.3} />
            <p>尚未触发压缩</p>
            <small>
              达到阈值后，将在这里记录摘要
              <br />
              与上下文用量变化。
            </small>
          </div>
        )}
      </section>
    </>
  )
}

export function ContextInspector({
  run,
  windowTokens,
  compactionAction,
}: {
  run?: RunDetail
  windowTokens?: number
  compactionAction?: ReactNode
}) {
  const tab = useUiStore((state) => state.inspectorTab)
  const setTab = useUiStore((state) => state.setInspectorTab)
  const close = useUiStore((state) => state.setInspectorOpen)
  const [sourceSequence, setSourceSequence] = useState<number | null>(null)
  useEffect(() => {
    if (tab !== 'timeline' || sourceSequence === null) return
    const item = document.getElementById(`timeline-event-${sourceSequence}`)
    if (item) {
      const detail = item.querySelector('details')
      if (detail) detail.open = true
      item.scrollIntoView({ block: 'center' })
      item.focus({ preventScroll: true })
    }
    setSourceSequence(null)
  }, [tab, sourceSequence])
  return (
    <aside className="inspector" aria-label="运行检查器">
      <div className="inspector-header">
        <span>运行检查器</span>
        <button className="icon-button" aria-label="关闭运行检查器" onClick={() => close(false)}>
          <PanelRightClose size={15} />
        </button>
      </div>
      <div className="inspector-tabs" role="tablist" aria-label="检查器内容">
        <button
          id="context-tab"
          role="tab"
          aria-controls="context-panel"
          aria-selected={tab === 'context'}
          className={tab === 'context' ? 'is-active' : ''}
          onClick={() => setTab('context')}
        >
          <Layers3 size={13} />
          上下文
        </button>
        <button
          id="timeline-tab"
          role="tab"
          aria-controls="timeline-panel"
          aria-selected={tab === 'timeline'}
          className={tab === 'timeline' ? 'is-active' : ''}
          onClick={() => setTab('timeline')}
        >
          <Clock3 size={13} />
          时间线{Boolean(run?.events.length) && <span>{run?.events.length}</span>}
        </button>
        <button
          id="memory-tab"
          role="tab"
          aria-controls="memory-panel"
          aria-selected={tab === 'memory'}
          className={tab === 'memory' ? 'is-active' : ''}
          onClick={() => setTab('memory')}
        >
          <BookOpen size={13} />
          记忆
        </button>
      </div>
      <div
        className="inspector-scroll"
        role="tabpanel"
        id={`${tab}-panel`}
        aria-labelledby={`${tab}-tab`}
      >
        {tab === 'context' ? (
          run?.context ? (
            <ContextUsage context={run.context} compactionAction={compactionAction} />
          ) : (
            <>
              <section className="inspector-section">
                <div className="section-eyebrow">
                  <Maximize2 size={14} />
                  模型容量
                </div>
                <div className="window-preview">
                  <strong>{windowTokens ? compactNumber(windowTokens) : '—'}</strong>
                  <span>tokens</span>
                </div>
                <p className="inspector-description">
                  运行开始后，你可以实时观察上下文预算、压缩触发点与历史保留情况。
                </p>
                {compactionAction && <div className="mt-4">{compactionAction}</div>}
              </section>
              <section className="inspector-section">
                <div className="section-eyebrow">
                  <Layers3 size={14} />
                  压缩检查点
                </div>
                <div className="inspector-empty">
                  <CircleDashed size={24} strokeWidth={1.3} />
                  <p>等待新的运行</p>
                  <small>每一次压缩，都会留下记录。</small>
                </div>
              </section>
            </>
          )
        ) : tab === 'memory' ? (
          <MemoryInspector
            run={run}
            onSource={(sequence) => {
              setSourceSequence(sequence)
              setTab('timeline')
            }}
          />
        ) : run?.events.length ? (
          <ol className="timeline">
            {run.events.map((event) => (
              <li key={event.sequence} id={`timeline-event-${event.sequence}`} tabIndex={-1}>
                <span className="timeline-dot" />
                <div className="timeline-heading">
                  <strong>{event.label}</strong>
                  <time>{time(event.timestamp)}</time>
                </div>
                <span className="timeline-step">
                  STEP {String(event.step).padStart(2, '0')} · #{event.sequence}
                </span>
                {event.detail && (
                  <details className="timeline-detail">
                    <summary>
                      查看事件详情
                      <ChevronRight size={11} />
                    </summary>
                    <p>{event.detail}</p>
                  </details>
                )}
              </li>
            ))}
          </ol>
        ) : (
          <div className="inspector-empty timeline-empty">
            <Clock3 size={25} strokeWidth={1.3} />
            <p>时间线准备就绪</p>
            <small>
              模型响应、工具执行和运行状态
              <br />
              将在这里依次呈现。
            </small>
          </div>
        )}
      </div>
      <div className="inspector-bottom">
        <span className="connection-dot" />
        {run ? '数据来自本地运行服务' : '运行后显示实际数据'}
      </div>
    </aside>
  )
}
