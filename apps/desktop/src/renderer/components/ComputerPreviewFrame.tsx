import type { ComputerPreview } from '../../shared/computer-preview'

export function ComputerPreviewFrame({
  source,
  width,
  height,
  rear = false,
}: {
  source: ComputerPreview
  width: number
  height: number
  rear?: boolean
}) {
  const scale = Math.min(width / (source.width ?? 1), height / (source.height ?? 1))
  const cursor = rear ? undefined : source.pointer
  return (
    <div className="computer-preview-frame" style={{ height }}>
      {source.jpeg && !source.unavailable ? (
        <img
          src={`data:image/jpeg;base64,${source.jpeg}`}
          alt={rear ? '' : `${source.title ?? '当前窗口'} 的实时操作画面`}
          draggable={false}
        />
      ) : (
        <span className="computer-preview-placeholder">
          {!rear && (source.unavailable ? '暂时无法预览此窗口' : '正在连接画面…')}
        </span>
      )}
      {cursor && !source.unavailable && (
        <svg
          key={`${source.id}:${cursor.sequence}`}
          className="computer-preview-cursor"
          style={{
            left: (width - (source.width ?? 0) * scale) / 2 + cursor.x * scale - 25,
            top: (height - (source.height ?? 0) * scale) / 2 + cursor.y * scale - 24,
          }}
          width="64"
          height="64"
          viewBox="0 0 64 64"
          aria-hidden="true"
        >
          <defs>
            <radialGradient id="preview-cursor-glow">
              <stop stopColor="#454545" stopOpacity=".2" />
              <stop offset="1" stopColor="#454545" stopOpacity="0" />
            </radialGradient>
          </defs>
          <circle cx="29" cy="29" r="29" fill="url(#preview-cursor-glow)" />
          {cursor.pressed && (
            <circle
              className="computer-preview-click"
              cx="25"
              cy="24"
              r="7"
              fill="none"
              stroke="#aaa"
            />
          )}
          <path
            d="M25 24 L27 46 L32 40 L36 47 L39 45 L35 38 L43 36 Z"
            fill="#888"
            stroke="white"
            strokeWidth="1.3"
            strokeLinejoin="round"
          />
        </svg>
      )}
    </div>
  )
}
