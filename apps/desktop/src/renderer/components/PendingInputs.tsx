import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  CornerDownRight,
  ListEnd,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  Trash2,
} from 'lucide-react'
import type { PendingInput, PendingInputAction, RunDetail } from '../../shared/contracts'
import { storeSnapshot } from '../lib/queries'
import { errorMessage } from '../lib/format'
import { ComposerMenu } from './ComposerMenu'
import { InlineError } from './Feedback'

export function PendingInputs({ run, disabled }: { run: RunDetail; disabled?: boolean }) {
  const client = useQueryClient()
  const [editing, setEditing] = useState<{ input: PendingInput; text: string } | null>(null)
  const change = useMutation({
    mutationFn: (action: PendingInputAction) => window.harness.updatePendingInput(run.id, action),
    onSuccess: (snapshot) => {
      storeSnapshot(client, snapshot)
      setEditing(null)
    },
    onSettled: () => void client.invalidateQueries({ queryKey: ['run', run.id] }),
  })
  if (!run.pending_inputs?.length) return null
  return (
    <div className="pending-inputs" aria-label="待发送消息" title="消息已暂存，仅在点击引导后发送">
      {run.pending_inputs.map((input) => {
        const busy = disabled || change.isPending || input.delivering
        const label = input.content || input.files.map((file) => file.name).join('、')
        return (
          <div className="pending-input" key={input.id}>
            {editing?.input.id === input.id ? (
              <form
                className="pending-input-editor"
                onSubmit={(event) => {
                  event.preventDefault()
                  change.mutate({
                    action: 'edit',
                    id: input.id,
                    revision: editing.input.revision,
                    content: editing.text,
                  })
                }}
              >
                <textarea
                  autoFocus
                  aria-label="编辑待发送消息"
                  maxLength={4000}
                  value={editing.text}
                  onChange={(event) => setEditing({ ...editing, text: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === 'Escape') setEditing(null)
                  }}
                />
                <div className="pending-input-edit-actions">
                  <button type="button" className="quiet-button" onClick={() => setEditing(null)}>
                    取消
                  </button>
                  <button
                    type="submit"
                    className="quiet-button"
                    disabled={busy || (!editing.text.trim() && !input.files.length)}
                  >
                    保存
                  </button>
                </div>
              </form>
            ) : (
              <>
                <ListEnd size={16} aria-hidden="true" />
                <span className="pending-input-text" title={label}>
                  {label}
                </span>
                {input.content && input.files.length > 0 && (
                  <small>{input.files.length} 个附件</small>
                )}
                <button
                  className="quiet-button pending-steer"
                  disabled={busy || (input.steer && !run.pending_inputs_paused)}
                  title="尽快按这条消息调整当前任务"
                  onClick={() =>
                    change.mutate({ action: 'steer', id: input.id, revision: input.revision })
                  }
                >
                  {input.delivering || (input.steer && !run.pending_inputs_paused) ? (
                    <>
                      <LoaderCircle size={14} className="animate-spin" />
                      正在引导
                    </>
                  ) : (
                    <>
                      <CornerDownRight size={16} />
                      引导
                    </>
                  )}
                </button>
                <button
                  className="icon-button"
                  aria-label="移除待发送消息"
                  title="移除消息"
                  disabled={busy}
                  onClick={() =>
                    change.mutate({ action: 'remove', id: input.id, revision: input.revision })
                  }
                >
                  <Trash2 size={15} />
                </button>
                <ComposerMenu
                  label="消息操作"
                  className="icon-button"
                  disabled={busy}
                  trigger={<MoreHorizontal size={16} />}
                >
                  <button
                    type="button"
                    role="menuitem"
                    className="composer-menu-item"
                    onClick={() => setEditing({ input, text: input.content })}
                  >
                    <Pencil size={14} />
                    编辑消息
                  </button>
                </ComposerMenu>
              </>
            )}
          </div>
        )
      })}
      <p className="pending-input-hint">
        {run.pending_inputs_paused
          ? '任务已停止，消息已保留。点击引导后发送。'
          : '已暂存，点击引导后发送。'}
      </p>
      {change.error && <InlineError message={errorMessage(change.error)} />}
    </div>
  )
}
