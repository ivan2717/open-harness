import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { Layers2, Minimize2, PictureInPicture2 } from 'lucide-react'
import type { ComputerPreview as Preview } from '../../shared/computer-preview'
import { ComputerPreviewFrame } from './ComputerPreviewFrame'
import './computer-preview.css'

const WIDTH = 320
const STACK_STEP = 18
const STORAGE = 'openharness.preview.position'
export function clampPreview(
  point: { x: number; y: number },
  area: { width: number; height: number },
  size: { width: number; height: number },
) {
  return {
    x: Math.max(8, Math.min(point.x, area.width - size.width - 8)),
    y: Math.max(8, Math.min(point.y, area.height - size.height - 8)),
  }
}

export function ComputerPreview() {
  const [sources, setSources] = useState<Record<string, Preview>>({})
  const [selected, setSelected] = useState('')
  const [collapsed, setCollapsed] = useState(false)
  const [position, setPosition] = useState<{ x: number; y: number } | null>(null)
  const [area, setArea] = useState({ width: 1000, height: 600 })
  const panel = useRef<HTMLDivElement>(null)
  const drag = useRef<{
    id: number
    x: number
    y: number
    origin: { x: number; y: number }
    current: { x: number; y: number }
    moved: boolean
  } | null>(null)
  const suppressClick = useRef(false)
  const [dragging, setDragging] = useState(false)
  const entries = Object.values(sources)
  const source = sources[selected] ?? entries[0]
  const index = entries.findIndex((item) => item.id === source?.id)
  const following = [...entries.slice(index + 1), ...entries.slice(0, index)]
  const depth = Math.min(2, following.length)
  const width = Math.min(WIDTH, Math.max(160, area.width - 16))
  const height = Math.min(
    240,
    Math.max(80, area.height - depth * STACK_STEP - 16),
    source?.width && source.height ? (width * source.height) / source.width : 200,
  )
  const size = collapsed
    ? { width: 36, height: 36 }
    : { width, height: height + depth * STACK_STEP }
  const point = clampPreview(
    position ?? { x: area.width - size.width - 24, y: area.height - size.height - 24 },
    area,
    size,
  )

  useEffect(
    () =>
      window.harness.watchComputerPreview((value) => {
        setSources((current) => {
          if (!value.active) {
            const next = { ...current }
            delete next[value.id]
            return next
          }
          return {
            ...current,
            [value.id]: {
              ...current[value.id],
              ...value,
              // Hidden/rear sources publish metadata only. IPC can preserve an
              // explicit undefined jpeg; keep the last frame until a new one
              // arrives (unavailable still hides it, disconnect removes it).
              jpeg: value.jpeg ?? current[value.id]?.jpeg,
              ...(value.jpeg ? { unavailable: false } : {}),
            },
          }
        })
      }),
    [],
  )
  useEffect(() => {
    if (!source) return
    setSelected(source.id)
    // Rear cards retain their last frame; only the front card streams. A new
    // source or incoming frame never steals the user's selected preview.
    for (const id of Object.keys(sources))
      void window.harness.collapseComputerPreview(id, collapsed || id !== source.id).catch(() => {})
  }, [source?.id, Object.keys(sources).join(','), collapsed])
  useEffect(() => {
    try {
      const saved = JSON.parse(localStorage.getItem(STORAGE) ?? 'null')
      if (saved && Number.isFinite(saved.x) && Number.isFinite(saved.y)) setPosition(saved)
    } catch {
      /* Ignore obsolete layout preferences. */
    }
  }, [])
  useEffect(() => {
    const parent = panel.current?.parentElement
    if (!parent) return
    const observer = new ResizeObserver(([entry]) =>
      setArea({ width: entry.contentRect.width, height: entry.contentRect.height }),
    )
    observer.observe(parent)
    return () => observer.disconnect()
  }, [Boolean(source)])

  if (!source) return null
  const rememberPosition = (next: { x: number; y: number }) => {
    setPosition(next)
    try {
      localStorage.setItem(STORAGE, JSON.stringify(next))
    } catch {
      /* Layout is optional. */
    }
  }
  const beginDrag = (event: PointerEvent<HTMLElement>) => {
    const button = (event.target as Element).closest('button')
    if (event.button !== 0 || (button && button !== event.currentTarget)) return
    suppressClick.current = false
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = {
      id: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      origin: point,
      current: point,
      moved: false,
    }
  }
  const move = (event: PointerEvent<HTMLElement>) => {
    const gesture = drag.current
    if (!gesture || gesture.id !== event.pointerId) return
    const dx = event.clientX - gesture.x
    const dy = event.clientY - gesture.y
    if (!gesture.moved && Math.hypot(dx, dy) < 4) return
    gesture.moved = true
    suppressClick.current = true
    gesture.current = clampPreview(
      { x: gesture.origin.x + dx, y: gesture.origin.y + dy },
      area,
      size,
    )
    setDragging(true)
    setPosition(gesture.current)
  }
  const endDrag = (event: PointerEvent<HTMLElement>) => {
    const gesture = drag.current
    if (!gesture || gesture.id !== event.pointerId) return
    if (gesture.moved) rememberPosition(gesture.current)
    drag.current = null
    setDragging(false)
    if (event.currentTarget.hasPointerCapture(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId)
  }
  const moveWithKeyboard = (event: KeyboardEvent<HTMLElement>) => {
    const direction = {
      ArrowLeft: [-16, 0],
      ArrowRight: [16, 0],
      ArrowUp: [0, -16],
      ArrowDown: [0, 16],
    }[event.key]
    if (!direction || event.target !== event.currentTarget) return
    event.preventDefault()
    rememberPosition(
      clampPreview({ x: point.x + direction[0], y: point.y + direction[1] }, area, size),
    )
  }
  const dragHandlers = {
    onPointerDown: beginDrag,
    onPointerMove: move,
    onPointerUp: endDrag,
    onPointerCancel: endDrag,
    onLostPointerCapture: endDrag,
    onKeyDown: moveWithKeyboard,
  }
  return (
    <div
      ref={panel}
      className={`computer-preview ${collapsed ? 'is-collapsed' : ''} ${dragging ? 'is-dragging' : ''}`}
      style={{ left: point.x, top: point.y, ...size }}
      role="region"
      aria-label="电脑操作预览"
    >
      {collapsed ? (
        <button
          className="computer-preview-restore"
          aria-label="展开操作预览"
          title="展开预览 · 可拖动位置"
          {...dragHandlers}
          onClick={(event) => {
            // A drag ends with a click on the captured button. Keep it collapsed;
            // a subsequent click or keyboard activation can still expand it.
            if (event.detail > 0 && suppressClick.current) {
              suppressClick.current = false
              return
            }
            setCollapsed(false)
          }}
        >
          {entries.length > 1 ? <Layers2 size={17} /> : <PictureInPicture2 size={18} />}
        </button>
      ) : (
        <>
          {following.slice(0, depth).map((item, offset) => (
            <button
              key={item.id}
              className="computer-preview-card computer-preview-back"
              style={{
                left: (offset + 1) * 8,
                top: (depth - offset - 1) * STACK_STEP,
                width: width - (offset + 1) * 16,
                height,
                zIndex: depth - offset,
              }}
              aria-label={`查看 ${item.title ?? '其他窗口'} 的预览`}
              onClick={() => setSelected(item.id)}
            >
              <div aria-hidden="true">
                <ComputerPreviewFrame
                  source={item}
                  width={width - (offset + 1) * 16}
                  height={height}
                  rear
                />
              </div>
            </button>
          ))}
          <div
            className="computer-preview-card computer-preview-front"
            style={{ top: depth * STACK_STEP, height, zIndex: depth + 1 }}
            {...dragHandlers}
            tabIndex={0}
            aria-label="拖动预览位置，也可使用方向键移动"
          >
            <ComputerPreviewFrame source={source} width={width} height={height} />
            <button
              className="computer-preview-control computer-preview-collapse"
              aria-label="收起操作预览"
              title="收起预览，任务继续运行"
              onClick={() => setCollapsed(true)}
            >
              <Minimize2 size={14} />
            </button>
            {entries.length > 1 && (
              <button
                className="computer-preview-control computer-preview-cycle"
                aria-label={`切换预览，当前第 ${index + 1} 个，共 ${entries.length} 个`}
                title="切换预览"
                onClick={() => setSelected(following[0].id)}
              >
                <Layers2 size={14} />
                <span aria-hidden="true">
                  {index + 1} / {entries.length}
                </span>
              </button>
            )}
          </div>
        </>
      )}
    </div>
  )
}
