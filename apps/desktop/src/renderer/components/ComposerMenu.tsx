import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

export function ComposerMenu({
  label,
  trigger,
  children,
  disabled,
  title,
  align = 'start',
  className = '',
}: {
  label: string
  trigger: ReactNode
  children: ReactNode
  disabled?: boolean
  title?: string
  align?: 'start' | 'end'
  className?: string
}) {
  const id = useId()
  const button = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const focusLast = useRef(false)
  const [open, setOpen] = useState(false)
  const expanded = open && !disabled

  function close(restoreFocus = false) {
    setOpen(false)
    if (restoreFocus) button.current?.focus()
  }

  useLayoutEffect(() => {
    if (!expanded || !menu.current || !button.current) return
    const panel = menu.current
    const anchor = button.current.getBoundingClientRect()
    const width = panel.offsetWidth
    const height = panel.offsetHeight
    const left = align === 'end' ? anchor.right - width : anchor.left
    const above = anchor.top - height - 8
    panel.style.left = `${Math.max(8, Math.min(left, window.innerWidth - width - 8))}px`
    panel.style.top = `${Math.max(8, Math.min(above >= 8 ? above : anchor.bottom + 8, window.innerHeight - height - 8))}px`
    const items = panel.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)')
    const selected = panel.querySelector<HTMLElement>('[aria-checked="true"]:not(:disabled)')
    const target = focusLast.current ? items[items.length - 1] : (selected ?? items[0])
    target?.focus()
  }, [expanded, align])

  useEffect(() => {
    if (!expanded) return
    const outside = (event: PointerEvent) => {
      const target = event.target as Node
      if (!button.current?.contains(target) && !menu.current?.contains(target)) setOpen(false)
    }
    const onResize = () => setOpen(false)
    document.addEventListener('pointerdown', outside, true)
    window.addEventListener('resize', onResize)
    return () => {
      document.removeEventListener('pointerdown', outside, true)
      window.removeEventListener('resize', onResize)
    }
  }, [expanded])

  return (
    <>
      <button
        ref={button}
        type="button"
        className={`composer-control ${className}`}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={expanded}
        aria-controls={expanded ? id : undefined}
        disabled={disabled}
        title={title}
        onClick={() => {
          focusLast.current = false
          setOpen(!expanded)
        }}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault()
            focusLast.current = event.key === 'ArrowUp'
            setOpen(true)
          }
        }}
      >
        {trigger}
      </button>
      {expanded &&
        createPortal(
          <div
            ref={menu}
            id={id}
            role="menu"
            aria-label={label}
            className="composer-menu"
            onClick={(event) => {
              const item = (event.target as HTMLElement).closest('[role^="menuitem"]')
              if (item && !item.matches(':disabled')) close(true)
            }}
            onKeyDown={(event) => {
              if (event.key === 'Escape') {
                event.preventDefault()
                event.stopPropagation()
                close(true)
              } else if (event.key === 'Tab') {
                close(true)
              } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
                event.preventDefault()
                const items = Array.from(
                  menu.current!.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)'),
                )
                const current = items.indexOf(document.activeElement as HTMLElement)
                const next =
                  event.key === 'Home'
                    ? 0
                    : event.key === 'End'
                      ? items.length - 1
                      : (current + (event.key === 'ArrowDown' ? 1 : -1) + items.length) %
                        items.length
                items[next]?.focus()
              }
            }}
          >
            {children}
          </div>,
          document.body,
        )}
    </>
  )
}
