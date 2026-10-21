import { AlertCircle, LoaderCircle, RotateCcw } from 'lucide-react'
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
  return (
    <div className="empty-feedback">
      <span className="error-symbol">
        <AlertCircle size={25} />
      </span>
      <h2>暂时无法连接工作区</h2>
      <p className="max-w-md break-words">{errorMessage(error)}</p>
      <button className="button button-secondary" onClick={retry}>
        <RotateCcw size={14} />
        重新连接
      </button>
    </div>
  )
}

export function InlineError({ message, retry }: { message: string; retry?: () => void }) {
  return (
    <div className="inline-error" role="alert">
      <AlertCircle size={15} className="shrink-0" />
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {retry && (
        <button onClick={retry} className="shrink-0 font-medium underline underline-offset-4">
          重试
        </button>
      )}
    </div>
  )
}
