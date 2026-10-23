import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import type { ReactNode } from 'react'

export function ControlTooltip({
  disabled,
  content,
  children,
}: {
  disabled?: boolean
  content: ReactNode
  children: (descriptionId: string | undefined) => ReactNode
}) {
  const id = useId()
  const button = useRef<HTMLSpanElement>(null)
  const tooltip = useRef<HTMLDivElement>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const [open, setOpen] = useState(false)
  const visible = open && !disabled

  function cancelTimer() {
    clearTimeout(timer.current)
  }

  function close() {
    cancelTimer()
    setOpen(false)
  }

  function show(delay = 250) {
    cancelTimer()
    if (disabled) return
    if (delay === 0) setOpen(true)
    else timer.current = setTimeout(() => setOpen(true), delay)
  }

  function leave() {
    cancelTimer()
    timer.current = setTimeout(() => setOpen(false), 100)
  }

  useEffect(() => () => clearTimeout(timer.current), [])
  useEffect(() => {
    if (disabled) close()
  }, [disabled])

  useLayoutEffect(() => {
    if (!visible || !button.current || !tooltip.current) return
    const anchor = button.current.getBoundingClientRect()
    const panel = tooltip.current
    const left = anchor.left + anchor.width / 2 - panel.offsetWidth / 2
    const above = anchor.top - panel.offsetHeight - 8
    panel.style.left = `${Math.max(8, Math.min(left, window.innerWidth - panel.offsetWidth - 8))}px`
    panel.style.top = `${Math.max(8, Math.min(above >= 8 ? above : anchor.bottom + 8, window.innerHeight - panel.offsetHeight - 8))}px`
  }, [visible])

  useEffect(() => {
    if (!visible) return
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close()
    }
    window.addEventListener('keydown', escape)
    window.addEventListener('resize', close)
    window.addEventListener('scroll', close, true)
    return () => {
      window.removeEventListener('keydown', escape)
      window.removeEventListener('resize', close)
      window.removeEventListener('scroll', close, true)
    }
  }, [visible])

  return (
    <>
      <span
        ref={button}
        className="control-tooltip-anchor"
        onPointerEnter={() => show()}
        onPointerLeave={leave}
        onFocus={() => show(0)}
        onBlur={close}
      >
        {children(visible ? id : undefined)}
      </span>
      {visible &&
        createPortal(
          <div
            ref={tooltip}
            id={id}
            role="tooltip"
            className="composer-attach-tooltip"
            onPointerEnter={() => show(0)}
            onPointerLeave={leave}
          >
            {content}
          </div>,
          document.body,
        )}
    </>
  )
}
