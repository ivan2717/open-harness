import { useEffect, useId, useRef, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { X } from 'lucide-react'

export function MessagePreviewDialog({
  title,
  onClose,
  children,
  actions,
}: {
  title: string
  onClose: () => void
  children: ReactNode
  actions?: ReactNode
}) {
  const id = useId()
  const dialog = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      element?.close()
      if (previous?.isConnected) previous.focus()
    }
  }, [])
  return createPortal(
    <dialog
      ref={dialog}
      className="message-preview-dialog"
      aria-labelledby={id}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return
        const bounds = event.currentTarget.getBoundingClientRect()
        if (
          event.clientX < bounds.left ||
          event.clientX > bounds.right ||
          event.clientY < bounds.top ||
          event.clientY > bounds.bottom
        )
          onClose()
      }}
    >
      <header>
        <span id={id}>{title}</span>
        <button type="button" className="icon-button" aria-label="关闭预览" onClick={onClose}>
          <X size={18} />
        </button>
      </header>
      <div className="message-preview-content">{children}</div>
      {actions && <footer>{actions}</footer>}
    </dialog>,
    document.body,
  )
}
