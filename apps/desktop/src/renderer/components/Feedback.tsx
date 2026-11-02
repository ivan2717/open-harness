import { AlertCircle, LoaderCircle, RotateCcw } from 'lucide-react'
import { useState } from 'react'
import { errorMessage } from '../lib/format'

export function Loading({ label = '正在加载工作区' }: { label?: string }) {
  return (
    <div className="empty-feedback">
      <LoaderCircle size={22} className="animate-spin text-muted" />
      <p>{label}</p>
    </div>
  )
}

export function ErrorState({ error, retry }: { error: unknown; retry: () => void }) {
  const message = errorMessage(error)
  return (
    <div className="empty-feedback">
      <span className="error-symbol">
        <AlertCircle size={25} />
      </span>
      <h2>暂时无法连接工作区</h2>
      <p className="max-w-md break-words">{message}</p>
      {message.includes('请重新启动 Open Harness') ? (
        <RuntimeRecovery onRecovered={retry} />
      ) : (
        <button className="button button-secondary" onClick={retry}>
          <RotateCcw size={14} />
          重新连接
        </button>
      )}
    </div>
  )
}

export function InlineError({ message, retry }: { message: string; retry?: () => void }) {
  return (
    <div className="inline-error" role="alert">
      <AlertCircle size={15} className="shrink-0" />
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {message.includes('请重新启动 Open Harness') ? (
        <RuntimeRecovery onRecovered={retry} />
      ) : (
        retry && (
          <button onClick={retry} className="shrink-0 font-medium underline underline-offset-4">
            重试
          </button>
        )
      )}
    </div>
  )
}

function RuntimeRecovery({ onRecovered }: { onRecovered?: () => void }) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  return (
    <span>
      <button
        type="button"
        className="button button-secondary"
        disabled={busy}
        onClick={async () => {
          setBusy(true)
          setError('')
          try {
            await window.harness.restartRuntime()
            onRecovered?.()
          } catch (error) {
            setError(errorMessage(error))
          } finally {
            setBusy(false)
          }
        }}
      >
        {busy ? '正在重启服务…' : '重启本地服务'}
      </button>
      {error && <span role="alert">{error}</span>}
    </span>
  )
}
