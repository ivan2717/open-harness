import { constants } from 'node:fs'
import { lstat, open, realpath } from 'node:fs/promises'
import { basename, extname, isAbsolute } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { FileImportResult, FileReference, StoredFileReference } from '../shared/contracts'
import { imageReference, IMAGE_EXTENSIONS } from './image-references'
import {
  MAX_REFERENCE_FILES,
  MAX_ATTACHMENT_SOURCE_BYTES,
  MAX_IMAGE_REFERENCE_FILES,
  MAX_IMAGE_SOURCE_BYTES,
} from '../shared/file-references'

const MAX_URI_LIST_BYTES = 64 * 1024
function validName(name: string): boolean {
  return (
    Boolean(name.trim()) &&
    [...name].length <= 255 &&
    !/[\/\\\u0000-\u001f\u007f-\u009f]/.test(name)
  )
}

/**
 * Electron maps Finder/Explorer's native copied-file formats to text/uri-list.
 * Read that main-process representation; renderer ClipboardEvent File objects
 * can have their disk paths removed by Chromium.
 */
export function clipboardFilePaths(uriList: string): string[] {
  if (Buffer.byteLength(uriList, 'utf8') > MAX_URI_LIST_BYTES)
    throw new Error('剪贴板文件列表过大，请分批添加。')
  const paths: string[] = []
  for (const line of uriList.split(/\r?\n/)) {
    const value = line.trim()
    if (!value || value.startsWith('#')) continue
    let url: URL
    try {
      url = new URL(value)
    } catch {
      throw new Error('剪贴板中的文件地址无效。')
    }
    if (
      url.protocol !== 'file:' ||
      (url.hostname && url.hostname !== 'localhost') ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      throw new Error('只能引用此计算机上的本地文件。')
    let path: string
    try {
      path = fileURLToPath(url)
    } catch {
      throw new Error('剪贴板中的文件地址无效。')
    }
    if (/[\u0000-\u001f\u007f-\u009f]/.test(path) || !isAbsolute(path))
      throw new Error('剪贴板中的文件地址无效。')
    if (!paths.includes(path)) paths.push(path)
    if (paths.length > MAX_REFERENCE_FILES) throw new Error('每次最多添加 64 个文件。')
  }
  return paths
}

interface FileClipboardItem {
  types: readonly string[]
  getType(type: 'text/uri-list'): Promise<unknown>
}

export type FileImporter = (name: string, bytes: Buffer) => Promise<StoredFileReference>

interface ClipboardSelection {
  files: { name: string; size: number; image: boolean }[]
  skipImages: boolean
}

function clipboardSelection(value: unknown): ClipboardSelection | undefined {
  if (value === undefined) return undefined
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('剪贴板文件选择无效。')
  const selection = value as Record<string, unknown>
  if (
    !Array.isArray(selection.files) ||
    selection.files.length > MAX_REFERENCE_FILES ||
    typeof selection.skipImages !== 'boolean'
  )
    throw new Error('剪贴板文件选择无效。')
  const files = selection.files.map((item: unknown) => {
    if (!item || typeof item !== 'object' || Array.isArray(item))
      throw new Error('剪贴板文件选择无效。')
    const file = item as Record<string, unknown>
    if (
      typeof file.name !== 'string' ||
      !validName(file.name) ||
      typeof file.size !== 'number' ||
      !Number.isSafeInteger(file.size) ||
      file.size < 0 ||
      typeof file.image !== 'boolean'
    )
      throw new Error('剪贴板文件选择无效。')
    return { name: file.name, size: file.size, image: file.image }
  })
  return { files, skipImages: selection.skipImages }
}

/** Match the captured paste event before reading bytes from the live system clipboard paths. */
async function selectedClipboardPaths(
  paths: string[],
  selection?: ClipboardSelection,
): Promise<string[]> {
  if (!selection) return paths
  if (paths.length !== selection.files.length) return []
  const names = selection.files.map((file) => file.name).sort()
  if (
    paths
      .map((path) => basename(path))
      .sort()
      .some((name, index) => name !== names[index])
  )
    return []
  const remaining = [...selection.files]
  const selected: string[] = []
  for (const path of paths) {
    const metadata = await lstat(path)
    const index = remaining.findIndex(
      (file) => file.name === basename(path) && file.size === metadata.size,
    )
    if (index < 0 || !metadata.isFile()) return []
    const [file] = remaining.splice(index, 1)
    if (!selection.skipImages || !file.image) selected.push(path)
  }
  return selected
}

export async function importClipboardFiles(
  read: () => Promise<FileClipboardItem[]>,
  importAttachment?: FileImporter,
  requestedSelection?: unknown,
): Promise<FileImportResult> {
  try {
    const selection = clipboardSelection(requestedSelection)
    const paths: string[] = []
    for (const item of await read()) {
      if (!item.types.includes('text/uri-list')) continue
      const blob = await item.getType('text/uri-list')
      if (!(blob instanceof Blob)) throw new Error('剪贴板中的文件列表无效。')
      if (blob.size > MAX_URI_LIST_BYTES) throw new Error('剪贴板文件列表过大，请分批添加。')
      paths.push(...clipboardFilePaths(await blob.text()))
    }
    return await importFilePaths(
      await selectedClipboardPaths([...new Set(paths)], selection),
      importAttachment,
    )
  } catch (error) {
    return { files: [], errors: [importError(error)] }
  }
}

/** Read one user-selected regular file, with a hard bound even if it grows mid-read. */
async function readReference(
  path: string,
  importAttachment?: FileImporter,
): Promise<FileReference> {
  const name = basename(path)
  if (!validName(name)) throw new Error('文件名称无效。')
  const extension = extname(name).toLowerCase()
  const image = IMAGE_EXTENSIONS.has(extension)
  if (!image && !importAttachment) throw new Error('附件存储尚未就绪，请重新启动应用。')
  const maximum = image ? MAX_IMAGE_SOURCE_BYTES : MAX_ATTACHMENT_SOURCE_BYTES
  const sizeError = image ? '图片超过 32 MiB，请先缩小图片。' : '文件超过 100 MiB，请先缩小文件。'
  const before = await lstat(path)
  if (!before.isFile()) throw new Error('请添加普通文件，不支持文件夹或符号链接。')
  const canonicalPath = await realpath(path)
  if ([...canonicalPath].length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(canonicalPath))
    throw new Error('文件路径无效。')
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const metadata = await handle.stat()
    if (!metadata.isFile()) throw new Error('请添加普通文件，不支持文件夹或符号链接。')
    if (metadata.size > maximum) throw new Error(sizeError)
    const buffer = Buffer.alloc(metadata.size + 1)
    let length = 0
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, length)
      if (!bytesRead) break
      length += bytesRead
    }
    if (length > maximum) throw new Error(sizeError)
    if (image) return { ...imageReference(name, buffer.subarray(0, length)), path: canonicalPath }
    const after = await handle.stat()
    if (
      length !== metadata.size ||
      after.size !== metadata.size ||
      after.mtimeMs !== metadata.mtimeMs ||
      after.ctimeMs !== metadata.ctimeMs
    )
      throw new Error('文件正在变化，请稍后重试。')
    const file = await importAttachment!(name, buffer.subarray(0, length))
    return { ...file, name, path: canonicalPath }
  } finally {
    await handle.close()
  }
}

function importError(error: unknown): string {
  const code = (error as NodeJS.ErrnoException)?.code
  if (code === 'ENOENT') return '文件不存在或已经移动。'
  if (code === 'EACCES' || code === 'EPERM') return '没有读取此文件的权限。'
  if (code === 'ELOOP') return '不支持符号链接，请选择原始文件。'
  if (code) return '无法读取此文件，请确认它是可访问的本地文件。'
  return error instanceof Error ? error.message : '无法读取此文件。'
}

export async function importFilePaths(
  value: unknown,
  importAttachment?: FileImporter,
): Promise<FileImportResult> {
  if (!Array.isArray(value) || value.length > MAX_REFERENCE_FILES)
    throw new Error('每次最多添加 64 个文件。')
  const files: FileReference[] = []
  const errors: string[] = []
  const seen = new Set<string>()
  let imageCount = 0
  for (const path of new Set(value)) {
    if (
      typeof path !== 'string' ||
      !isAbsolute(path) ||
      /[\u0000-\u001f\u007f-\u009f]/.test(path) ||
      [...path].length > 4096
    ) {
      errors.push('无法读取文件来源，请重新复制后粘贴，或使用“添加文件”。')
      continue
    }
    try {
      const file = await readReference(path, importAttachment)
      if (seen.has(file.path!)) continue
      seen.add(file.path!)
      if ('image' in file && file.image) {
        if (imageCount >= MAX_IMAGE_REFERENCE_FILES) throw new Error('每条消息最多添加 4 张图片。')
        imageCount++
        files.push(file)
        continue
      }
      files.push(file)
    } catch (error) {
      errors.push(`${basename(path)}：${importError(error)}`)
    }
  }
  return { files, errors }
}
