import { useState } from 'react'
import type { FileImportResult } from '../../shared/contracts'
import { MAX_REFERENCE_FILES, MAX_REFERENCE_TOTAL_BYTES } from '../../shared/file-references'
import { useUiStore, type DraftFileReference } from '../state/ui-store'
import { errorMessage } from './format'

const emptyFiles: DraftFileReference[] = []
const encoder = new TextEncoder()

export function useFileReferences(draftKey: string) {
  const files = useUiStore((state) => state.draftFiles[draftKey] ?? emptyFiles)
  const pending = useUiStore((state) => Boolean(state.fileImports[draftKey]))
  const [error, setError] = useState<string | null>(null)

  async function importReferences(read: () => Promise<FileImportResult>): Promise<boolean> {
    if (useUiStore.getState().fileImports[draftKey]) return false
    useUiStore.getState().setFileImporting(draftKey, true)
    setError(null)
    try {
      const result = await read()
      const state = useUiStore.getState()
      const current = [...(state.draftFiles[draftKey] ?? [])]
      let size = current.reduce((total, file) => total + encoder.encode(file.text).byteLength, 0)
      const errors = [...result.errors]
      for (const file of result.files) {
        if (
          current.some((old) =>
            file.path ? old.path === file.path : old.name === file.name && old.text === file.text,
          )
        )
          continue
        const bytes = encoder.encode(file.text).byteLength
        if (current.length >= MAX_REFERENCE_FILES) {
          errors.push('每条消息最多引用 8 个文件。')
          break
        }
        if (size + bytes > MAX_REFERENCE_TOTAL_BYTES) {
          errors.push(`添加 ${file.name} 后超出 512 KB 总量，请移除部分文件。`)
          continue
        }
        size += bytes
        current.push({ ...file, id: crypto.randomUUID() })
      }
      state.setDraftFiles(draftKey, current)
      setError([...new Set(errors)].join(' ') || null)
      return result.files.length > 0 || result.errors.length > 0
    } catch (cause) {
      setError(errorMessage(cause))
      return true
    } finally {
      useUiStore.getState().setFileImporting(draftKey, false)
    }
  }

  return {
    files,
    pending,
    error,
    importReferences,
    remove: (index: number) => {
      useUiStore.getState().removeDraftFiles(draftKey, [files[index].id])
      setError(null)
    },
  }
}
