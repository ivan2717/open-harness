import { useState } from 'react'
import type { FileImportResult } from '../../shared/contracts'
import {
  MAX_REFERENCE_FILES,
  MAX_REFERENCE_TOTAL_BYTES,
  MAX_IMAGE_REFERENCE_FILES,
  fileReferenceSize,
} from '../../shared/file-references'
import { useUiStore, type DraftFileReference } from '../state/ui-store'
import { errorMessage } from './format'

const emptyFiles: DraftFileReference[] = []

export function mergeDraftReferences(
  existing: DraftFileReference[],
  result: FileImportResult,
): { files: DraftFileReference[]; errors: string[] } {
  const files = [...existing]
  let textSize = files.reduce(
    (total, file) => total + (file.image || file.attachment ? 0 : fileReferenceSize(file)),
    0,
  )
  let imageCount = files.filter((file) => file.image).length
  const errors = [...result.errors]
  for (const file of result.files) {
    if (
      files.some((old) =>
        file.path
          ? old.path === file.path
          : file.image
            ? old.image?.mime_type === file.image.mime_type &&
              old.image?.data_base64 === file.image.data_base64
            : file.attachment
              ? old.attachment?.id === file.attachment.id && old.name === file.name
              : !old.image && !old.attachment && old.name === file.name && old.text === file.text,
      )
    )
      continue
    if (files.length >= MAX_REFERENCE_FILES) {
      errors.push('每条消息最多添加 64 个附件。')
      break
    }
    if (file.image) {
      if (imageCount >= MAX_IMAGE_REFERENCE_FILES) {
        errors.push('每条消息最多添加 4 张图片。')
        continue
      }
      imageCount += 1
    } else if (!file.attachment) {
      const bytes = fileReferenceSize(file)
      if (textSize + bytes > MAX_REFERENCE_TOTAL_BYTES) {
        errors.push(`添加 ${file.name} 后正文超出 512 KB 总量，请移除部分文件。`)
        continue
      }
      textSize += bytes
    }
    files.push({ ...file, id: crypto.randomUUID() })
  }
  return { files, errors: [...new Set(errors)] }
}

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
      const merged = mergeDraftReferences(state.draftFiles[draftKey] ?? [], result)
      state.setDraftFiles(draftKey, merged.files)
      setError(merged.errors.join(' ') || null)
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
