import { useRef, useState } from 'react'
import { ArrowUp, ChevronDown, LoaderCircle, Square } from 'lucide-react'
import type { ModelInfo } from '../../shared/contracts'
import { useUiStore } from '../state/ui-store'

interface ComposerProps {
  draftKey: string
  models: ModelInfo[]
  modelId: string
  onModelChange?: (id: string) => void
  onSubmit: (prompt: string) => Promise<void>
  pending?: boolean
  running?: boolean
  onStop?: () => void
  stopping?: boolean
  waiting?: boolean
  disabled?: boolean
}

export function Composer({
  draftKey,
  models,
  modelId,
  onModelChange,
  onSubmit,
  pending,
  running,
  onStop,
  stopping,
  waiting,
  disabled,
}: ComposerProps) {
  const draft = useUiStore((state) => state.drafts[draftKey] ?? '')
  const setDraft = useUiStore((state) => state.setDraft)
  const clearDraft = useUiStore((state) => state.clearDraft)
  const textarea = useRef<HTMLTextAreaElement>(null)
  const [composing, setComposing] = useState(false)
  const model = models.find((item) => item.id === modelId)
  const canSubmit = Boolean(draft.trim()) && !pending && !running && !disabled

  async function submit() {
    if (!canSubmit) return
    const submittedDraft = draft
    try {
      await onSubmit(submittedDraft.trim())
      if (useUiStore.getState().drafts[draftKey] === submittedDraft) clearDraft(draftKey)
    } catch {
      // The owning mutation renders its error; keep the draft for retry.
    }
  }

  return (
    <div className={`composer ${waiting ? 'composer-waiting' : ''}`}>
      {waiting && (
        <div className="composer-hint">
          <span className="status-dot status-waiting_for_input" />
          当前运行正在等待你的补充
        </div>
      )}
      <textarea
        ref={textarea}
        rows={3}
        value={draft}
        disabled={disabled || running}
        onChange={(event) => setDraft(draftKey, event.target.value)}
        onCompositionStart={() => setComposing(true)}
        onCompositionEnd={() => setComposing(false)}
        onKeyDown={(event) => {
          if (
            event.key === 'Enter' &&
            !event.shiftKey &&
            !composing &&
            !event.nativeEvent.isComposing
          ) {
            event.preventDefault()
            void submit()
          }
        }}
        placeholder={
          running
            ? '正在执行，你可以随时停止运行…'
            : waiting
              ? '补充信息，继续这次运行…'
              : '描述你的任务…'
        }
        aria-label={waiting ? '补充信息' : '任务描述'}
      />
      <div className="composer-footer">
        <div className="flex min-w-0 items-center gap-2">
          <div
            className="model-select"
            title={
              modelId === 'local-demo' ? '离线演示，使用演示数据，不调用模型 API' : model?.name
            }
          >
            {onModelChange ? (
              <>
                <select
                  aria-label="选择模型"
                  value={modelId}
                  onChange={(event) => onModelChange(event.target.value)}
                  disabled={pending || running || !models.length}
                >
                  {models.map((item) => (
                    <option key={item.id} value={item.id} disabled={!item.available}>
                      {item.name}
                      {!item.available ? ' · 未配置' : ''}
                    </option>
                  ))}
                </select>
                <ChevronDown size={12} className="pointer-events-none shrink-0" />
              </>
            ) : (
              <span className="truncate">{model?.name ?? modelId}</span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-3">
          {running ? (
            <button
              className="send-button stop-button"
              onClick={onStop}
              disabled={stopping}
              aria-label="停止运行"
              title="停止运行"
            >
              {stopping ? (
                <LoaderCircle size={16} className="animate-spin" />
              ) : (
                <Square size={13} fill="currentColor" />
              )}
            </button>
          ) : (
            <button
              className="send-button"
              disabled={!canSubmit}
              onClick={() => void submit()}
              aria-label={waiting ? '提交补充信息' : '开始运行'}
              title="开始运行"
            >
              {pending ? (
                <LoaderCircle size={17} className="animate-spin" />
              ) : (
                <ArrowUp size={19} />
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
