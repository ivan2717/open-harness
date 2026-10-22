import { FileText, X } from 'lucide-react'
import type { FileReferenceInfo } from '../../shared/contracts'
import { LocalResourceLink } from './MessageResource'

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  return `${Math.ceil(bytes / 1024)} KB`
}

export function FileReferences({
  files,
  onRemove,
  disabled,
  runId,
  messageId,
}: {
  files: (FileReferenceInfo & { id?: string })[]
  onRemove?: (index: number) => void
  disabled?: boolean
  runId?: string
  messageId?: string
}) {
  if (!files.length) return null
  return (
    <ul className="file-references" aria-label="引用文件">
      {files.map((file, index) => (
        <li key={file.id ?? `${index}:${file.name}`} title={file.path ?? file.name}>
          {runId && messageId ? (
            <LocalResourceLink
              reference={{ runId, messageId, fileIndex: index }}
              className="file-reference-preview"
              ariaLabel={`预览文件 ${file.name}`}
            >
              <FileText size={14} aria-hidden="true" />
              <span className="file-reference-name">{file.name}</span>
              <span className="file-reference-size">{fileSize(file.size)}</span>
            </LocalResourceLink>
          ) : (
            <>
              <FileText size={14} aria-hidden="true" />
              <span className="file-reference-name">{file.name}</span>
              <span className="file-reference-size">{fileSize(file.size)}</span>
            </>
          )}
          {onRemove && (
            <button
              type="button"
              aria-label={`移除文件 ${file.name}`}
              title="移除引用"
              disabled={disabled}
              onClick={() => onRemove(index)}
            >
              <X size={12} />
            </button>
          )}
        </li>
      ))}
    </ul>
  )
}
