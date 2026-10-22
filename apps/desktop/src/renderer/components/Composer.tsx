import { useLayoutEffect, useRef, useState } from 'react'
import { ArrowUp, LoaderCircle, Square } from 'lucide-react'
import type { ModelInfo } from '../../shared/contracts'
import { useUiStore, type ProjectSelection } from '../state/ui-store'
import { errorMessage } from '../lib/format'
import { ComposerModel } from './ComposerModel'
import { InlineError } from './Feedback'
import { ProjectPicker } from './ProjectPicker'

interface ComposerProps {
  draftKey: string
  models: ModelInfo[]
  modelId: string
  onModelChange?: (id: string) => void
  project?: ProjectSelection | null
  onProjectChange?: (project: ProjectSelection | null) => void
  onSubmit: (prompt: string) => Promise<void>
  pending?: boolean
  running?: boolean
  onStop?: () => void
  stopping?: boolean
  waiting?: boolean
  disabled?: boolean
  submitDisabledReason?: string
}

export function Composer({
  draftKey,
  models,
  modelId,
  onModelChange,
  project,
  onProjectChange,
  onSubmit,
  pending,
  running,
  onStop,
  stopping,
  waiting,
  disabled,
  submitDisabledReason,
}: ComposerProps) {
  const draft = useUiStore((state) => state.drafts[draftKey] ?? '')
  const setDraft = useUiStore((state) => state.setDraft)
  const clearDraft = useUiStore((state) => state.clearDraft)
  const textarea = useRef<HTMLTextAreaElement>(null)
  const submitting = useRef(false)
  const [composing, setComposing] = useState(false)
  const [choosing, setChoosing] = useState(false)
  const [projectError, setProjectError] = useState<string | null>(null)
  const canSubmit =
    Boolean(draft.trim()) && !pending && !running && !disabled && !submitDisabledReason && !choosing
  const controlsBusy = pending || choosing

  useLayoutEffect(() => {
    const field = textarea.current
    if (!field) return
    const resize = () => {
      field.style.height = '0px'
      const height = field.scrollHeight
      field.style.height = `${Math.max(76, Math.min(220, height))}px`
      field.style.overflowY = height > 220 ? 'auto' : 'hidden'
    }
    resize()
    let width = field.getBoundingClientRect().width
    const observer = new ResizeObserver(() => {
      const next = field.getBoundingClientRect().width
      if (next !== width) {
        width = next
        resize()
      }
    })
    observer.observe(field)
    return () => observer.disconnect()
  }, [draft])

  async function chooseProject() {
    if (!onProjectChange || controlsBusy) return
    setChoosing(true)
    setProjectError(null)
    try {
      const path = await window.harness.chooseWorkspace()
      if (path)
        onProjectChange(path === project?.path ? project : { path, permission: 'read_only' })
    } catch (error) {
      setProjectError(errorMessage(error))
    } finally {
      setChoosing(false)
    }
  }

  async function submit() {
    if (!canSubmit || submitting.current) return
    submitting.current = true
    const submittedDraft = draft
    try {
      await onSubmit(submittedDraft.trim())
      if (useUiStore.getState().drafts[draftKey] === submittedDraft) clearDraft(draftKey)
    } catch {
      // The owning mutation renders its error; keep the draft for retry.
    } finally {
      submitting.current = false
    }
  }

  return (
    <div className="composer-stack">
      <ProjectPicker
        project={project}
        onChoose={onProjectChange ? () => void chooseProject() : undefined}
        onChange={onProjectChange}
        disabled={controlsBusy}
      />
      <div className={`composer ${waiting ? 'composer-waiting' : ''}`}>
        {waiting && (
          <div className="composer-hint">
            <span className="status-dot status-waiting_for_input" />
            等待你的补充
          </div>
        )}
        <textarea
          ref={textarea}
          rows={2}
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
              !event.nativeEvent.isComposing &&
              event.nativeEvent.keyCode !== 229
            ) {
              event.preventDefault()
              void submit()
            }
          }}
          placeholder={
            running
              ? '正在执行，你可以随时停止…'
              : waiting
                ? '补充信息，继续这次任务…'
                : '描述任务，或直接提问…'
          }
          aria-label={waiting ? '补充信息' : '任务描述'}
        />
        <div className="composer-footer">
          <div className="composer-footer-right">
            <ComposerModel
              models={models}
              modelId={modelId}
              onChange={onModelChange}
              disabled={controlsBusy || running}
            />
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
                title={submitDisabledReason ?? '发送 · Enter'}
                aria-description={submitDisabledReason}
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
      {projectError && <InlineError message={projectError} />}
    </div>
  )
}
