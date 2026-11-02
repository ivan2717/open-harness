import { useState } from 'react'
import { FileText, ImageIcon, X } from 'lucide-react'
import type { FileReferenceInfo } from '../../shared/contracts'
import { LocalResourceLink } from './MessageResource'
import { MessagePreviewDialog } from './MessagePreviewDialog'

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  return `${Math.ceil(bytes / 1024)} KB`
}

export function FileReferences({
  files,
  onRemove,
  disabled,
  runId,
  messageId,
}: {
  files: (FileReferenceInfo & { id?: string; preview?: string })[]
  onRemove?: (index: number) => void
  disabled?: boolean
  runId?: string
  messageId?: string
}) {
  if (!files.length) return null
  return (
    <ul className="file-references" aria-label="引用文件">
      {files.map((file, index) => (
        <li
          key={file.id ?? `${index}:${file.name}`}
          title={file.path ?? file.name}
          className={file.mime_type?.startsWith('image/') ? 'file-reference-image' : undefined}
        >
          {runId && messageId ? (
            <LocalResourceLink
              reference={{ runId, messageId, fileIndex: index }}
              className="file-reference-preview"
              ariaLabel={`预览文件 ${file.name}`}
              imagePreview={Boolean(file.mime_type?.startsWith('image/'))}
            >
              {!file.mime_type?.startsWith('image/') && <FileText size={14} aria-hidden="true" />}
              <span className="file-reference-name">{file.name}</span>
              <span className="file-reference-size">{fileSize(file.size)}</span>
            </LocalResourceLink>
          ) : file.preview ? (
            <DraftImagePreview name={file.name} source={file.preview} />
          ) : (
            <>
              {file.mime_type?.startsWith('image/') ? (
                <ImageIcon size={14} aria-hidden="true" />
              ) : (
                <FileText size={14} aria-hidden="true" />
              )}
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
              className="file-reference-remove"
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

function DraftImagePreview({ name, source }: { name: string; source: string }) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button
        type="button"
        className="file-reference-image-preview"
        aria-label={`预览图片 ${name}`}
        onClick={() => setOpen(true)}
      >
        <img className="file-reference-thumbnail" src={source} alt={name} />
        <span className="file-reference-name">{name}</span>
      </button>
      {open && (
        <MessagePreviewDialog title={name} onClose={() => setOpen(false)}>
          <img className="file-reference-full-image" src={source} alt={name} />
        </MessagePreviewDialog>
      )}
    </>
  )
}
