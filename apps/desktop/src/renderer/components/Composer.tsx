import { useLayoutEffect, useRef, useState, type ClipboardEvent, type DragEvent } from 'react'
import { ArrowUp, LoaderCircle, Square } from 'lucide-react'
import type { ModelInfo, TextFileReference } from '../../shared/contracts'
import { useUiStore, type ProjectSelection } from '../state/ui-store'
import { errorMessage } from '../lib/format'
import { ComposerModel } from './ComposerModel'
import { InlineError } from './Feedback'
import { ProjectPicker } from './ProjectPicker'
import { FileReferences } from './FileReferences'
import { useFileReferences } from '../lib/use-file-references'
import { ComposerAddMenu } from './ComposerAddMenu'
import { SkillPickerDialog } from './SkillPickerDialog'
import { insertSkillMention } from '../lib/skill-mention'

interface ComposerProps {
  draftKey: string
  models: ModelInfo[]
  modelId: string
  onModelChange?: (id: string) => void
  project?: ProjectSelection | null
  onProjectChange?: (project: ProjectSelection | null) => void
  onSubmit: (prompt: string, files: TextFileReference[]) => Promise<void>
  pending?: boolean
  running?: boolean
  onStop?: () => void
  stopping?: boolean
  disabled?: boolean
  submitDisabledReason?: string
  autoFocus?: boolean
  runId?: string
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
  disabled,
  submitDisabledReason,
  autoFocus,
  runId,
}: ComposerProps) {
  const draft = useUiStore((state) => state.drafts[draftKey] ?? '')
  const setDraft = useUiStore((state) => state.setDraft)
  const clearDraft = useUiStore((state) => state.clearDraft)
  const textarea = useRef<HTMLTextAreaElement>(null)
  const submitting = useRef(false)
  const [composing, setComposing] = useState(false)
  const [choosing, setChoosing] = useState(false)
  const [projectError, setProjectError] = useState<string | null>(null)
  const references = useFileReferences(draftKey)
  const dragDepth = useRef(0)
  const [dragging, setDragging] = useState(false)
  const [skillsOpen, setSkillsOpen] = useState(false)
  const skillCursor = useRef({ start: 0, end: 0 })
  const canSubmit =
    Boolean(draft.trim() || references.files.length) &&
    !pending &&
    !running &&
    !disabled &&
    !submitDisabledReason &&
    !choosing &&
    !references.pending
  const controlsBusy = pending || choosing || references.pending
  const filesDisabled = controlsBusy || disabled || running

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
    const submittedFiles = references.files
    try {
      await onSubmit(
        submittedDraft.trim(),
        submittedFiles.map(({ name, path, text }) => ({ name, path, text })),
      )
      if (useUiStore.getState().drafts[draftKey] === submittedDraft) clearDraft(draftKey)
      useUiStore.getState().removeDraftFiles(
        draftKey,
        submittedFiles.map((file) => file.id),
      )
    } catch {
      // The owning mutation renders its error; keep the draft for retry.
    } finally {
      submitting.current = false
    }
  }

  function isFileDrag(event: DragEvent) {
    return Array.from(event.dataTransfer.types).includes('Files')
  }

  function dropFiles(event: DragEvent) {
    if (!isFileDrag(event)) return
    event.preventDefault()
    dragDepth.current = 0
    setDragging(false)
    if (filesDisabled) return
    const files = Array.from(event.dataTransfer.files)
    void references.importReferences(() => window.harness.importFiles(files))
  }

  function pasteFiles(event: ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(event.clipboardData.files)
    const types = Array.from(event.clipboardData.types)
    const text = event.clipboardData.getData('text/plain')
    const uris = event.clipboardData.getData('text/uri-list')
    const hasFileUri = uris.split(/\r?\n/).some((line) => /^file:/i.test(line.trim()))
    // Ordinary text remains a native textarea paste, preserving selection/undo.
    if (!files.length && (text || uris) && !hasFileUri && !types.includes('Files')) return
    event.preventDefault()
    if (filesDisabled) return
    const start = event.currentTarget.selectionStart
    const end = event.currentTarget.selectionEnd
    void references
      .importReferences(
        files.length ? () => window.harness.importFiles(files) : () => window.harness.pasteFiles(),
      )
      .then((handled) => {
        if (!handled && text && (useUiStore.getState().drafts[draftKey] ?? '') === draft) {
          setDraft(draftKey, draft.slice(0, start) + text + draft.slice(end))
          requestAnimationFrame(() =>
            textarea.current?.setSelectionRange(start + text.length, start + text.length),
          )
        }
      })
  }

  return (
    <div className="composer-stack">
      <ProjectPicker
        project={project}
        onChoose={onProjectChange ? () => void chooseProject() : undefined}
        onChange={onProjectChange}
        disabled={controlsBusy}
      />
      <div
        className={`composer ${dragging ? 'composer-dragging' : ''}`}
        onDragEnter={(event) => {
          if (!isFileDrag(event)) return
          event.preventDefault()
          dragDepth.current += 1
          if (!filesDisabled) setDragging(true)
        }}
        onDragOver={(event) => {
          if (!isFileDrag(event)) return
          event.preventDefault()
          event.dataTransfer.dropEffect = filesDisabled ? 'none' : 'copy'
        }}
        onDragLeave={(event) => {
          if (!isFileDrag(event)) return
          dragDepth.current = Math.max(0, dragDepth.current - 1)
          if (!dragDepth.current) setDragging(false)
        }}
        onDrop={dropFiles}
      >
        {dragging && <div className="composer-drop-hint">松开以引用文件</div>}
        <FileReferences
          files={references.files.map((file) => ({
            ...file,
            size: new TextEncoder().encode(file.text).byteLength,
          }))}
          onRemove={references.remove}
          disabled={pending}
        />
        <textarea
          ref={textarea}
          autoFocus={autoFocus}
          rows={2}
          value={draft}
          disabled={disabled || running}
          onChange={(event) => setDraft(draftKey, event.target.value)}
          onPaste={pasteFiles}
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
          placeholder="描述任务，或直接提问…"
          aria-label="任务描述"
        />
        <div className="composer-footer">
          <ComposerAddMenu
            disabled={filesDisabled}
            pending={references.pending}
            onFiles={() => void references.importReferences(() => window.harness.chooseFiles())}
            onSkills={() => {
              skillCursor.current = {
                start: textarea.current?.selectionStart ?? draft.length,
                end: textarea.current?.selectionEnd ?? draft.length,
              }
              setSkillsOpen(true)
            }}
          />
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
                aria-label="开始运行"
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
      {references.error && <InlineError message={references.error} />}
      {projectError && <InlineError message={projectError} />}
      {skillsOpen && !filesDisabled && (
        <SkillPickerDialog
          runId={runId}
          workspacePath={project?.path}
          onClose={() => setSkillsOpen(false)}
          onChoose={(skill) => {
            const { text, cursor } = insertSkillMention(
              useUiStore.getState().drafts[draftKey] ?? '',
              skill.id,
              skillCursor.current.start,
              skillCursor.current.end,
            )
            setDraft(draftKey, text)
            setSkillsOpen(false)
            requestAnimationFrame(() => {
              textarea.current?.focus()
              textarea.current?.setSelectionRange(cursor, cursor)
            })
          }}
        />
      )}
    </div>
  )
}
