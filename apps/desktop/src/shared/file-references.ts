export const MAX_REFERENCE_FILES = 64
export const MAX_FILE_REFERENCE_BYTES = 256 * 1024
export const MAX_REFERENCE_TOTAL_BYTES = 512 * 1024
export const MAX_ATTACHMENT_SOURCE_BYTES = 100 * 1024 * 1024
export const MAX_DOCUMENT_SOURCE_BYTES = 32 * 1024 * 1024
export const DOCUMENT_EXTENSIONS = new Set(['.pdf', '.docx', '.xlsx', '.pptx'])
import type { FileReference } from './contracts'

export const MAX_IMAGE_REFERENCE_BYTES = 5 * 1024 * 1024
export const MAX_IMAGE_SOURCE_BYTES = 32 * 1024 * 1024
export const MAX_IMAGE_REFERENCE_PIXELS = 4_000_000
export const MAX_IMAGE_REFERENCE_EDGE = 8192
export const MAX_IMAGE_REFERENCE_FILES = 4

export function fileReferenceSize(file: FileReference): number {
  if (file.attachment) return file.attachment.size
  if (!file.image) return new TextEncoder().encode(file.text).byteLength
  const data = file.image.data_base64
  return (data.length / 4) * 3 - (data.endsWith('==') ? 2 : data.endsWith('=') ? 1 : 0)
}
