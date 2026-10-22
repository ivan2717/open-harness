import { constants } from 'node:fs'
import { lstat, open, realpath } from 'node:fs/promises'
import { basename, extname, isAbsolute } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { FileImportResult, TextFileReference } from '../shared/contracts'
import {
  MAX_REFERENCE_FILES,
  MAX_FILE_REFERENCE_BYTES as MAX_REFERENCE_BYTES,
  MAX_REFERENCE_TOTAL_BYTES,
} from '../shared/file-references'

const MAX_URI_LIST_BYTES = 64 * 1024
const binaryExtensions = new Set([
  '.pdf',
  '.png',
  '.jpg',
  '.jpeg',
  '.gif',
  '.webp',
  '.bmp',
  '.ico',
  '.tif',
  '.tiff',
  '.heic',
  '.avif',
  '.doc',
  '.docx',
  '.xls',
  '.xlsx',
  '.ppt',
  '.pptx',
  '.zip',
  '.gz',
  '.tar',
  '.7z',
  '.rar',
  '.woff',
  '.woff2',
  '.ttf',
  '.otf',
  '.mp3',
  '.mp4',
  '.mov',
  '.avi',
  '.exe',
  '.dll',
  '.dylib',
  '.so',
  '.wasm',
  '.sqlite',
  '.db',
])

function validName(name: string): boolean {
  return (
    Boolean(name.trim()) &&
    [...name].length <= 255 &&
    !/[\/\\\u0000-\u001f\u007f-\u009f]/.test(name)
  )
}

function ordinaryText(text: string): boolean {
  return (
    !/[\u0000-\u0008\u000b-\u000c\u000e-\u001f\u007f-\u009f]/.test(text) &&
    !text.startsWith('%PDF-')
  )
}

/** Only file URLs in the OS file clipboard are interpreted as file references. */
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
    if (paths.length > MAX_REFERENCE_FILES) throw new Error('每次最多添加 8 个文件。')
  }
  return paths
}

interface FileClipboardItem {
  types: readonly string[]
  getType(type: 'text/uri-list'): Promise<unknown>
}

export async function importClipboardFiles(
  read: () => Promise<FileClipboardItem[]>,
): Promise<FileImportResult> {
  try {
    const paths: string[] = []
    for (const item of await read()) {
      if (!item.types.includes('text/uri-list')) continue
      const blob = await item.getType('text/uri-list')
      if (!(blob instanceof Blob)) throw new Error('剪贴板中的文件列表无效。')
      if (blob.size > MAX_URI_LIST_BYTES) throw new Error('剪贴板文件列表过大，请分批添加。')
      paths.push(...clipboardFilePaths(await blob.text()))
    }
    return await importFilePaths([...new Set(paths)])
  } catch (error) {
    return { files: [], errors: [importError(error)] }
  }
}

/** Read one user-selected regular file, with a hard bound even if it grows mid-read. */
async function readReference(path: string): Promise<TextFileReference> {
  const name = basename(path)
  if (!validName(name)) throw new Error('文件名称无效。')
  if (binaryExtensions.has(extname(name).toLowerCase()))
    throw new Error('暂不支持此文件格式，请添加 UTF-8 文本或代码文件。')
  const before = await lstat(path)
  if (!before.isFile()) throw new Error('请添加普通文件，不支持文件夹或符号链接。')
  const canonicalPath = await realpath(path)
  if ([...canonicalPath].length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(canonicalPath))
    throw new Error('文件路径无效。')
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const metadata = await handle.stat()
    if (!metadata.isFile()) throw new Error('请添加普通文件，不支持文件夹或符号链接。')
    if (metadata.size > MAX_REFERENCE_BYTES) throw new Error('文件超过 256 KiB，请先缩小内容。')
    const buffer = Buffer.alloc(MAX_REFERENCE_BYTES + 1)
    let length = 0
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, length)
      if (!bytesRead) break
      length += bytesRead
    }
    if (length > MAX_REFERENCE_BYTES) throw new Error('文件超过 256 KiB，请先缩小内容。')
    let text: string
    try {
      text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(
        buffer.subarray(0, length),
      )
    } catch {
      throw new Error('文件不是有效的 UTF-8 文本，暂时无法引用。')
    }
    if (!ordinaryText(text)) throw new Error('文件包含二进制内容，暂时无法引用。')
    return { name, path: canonicalPath, text }
  } finally {
    await handle.close()
  }
}

function importError(error: unknown): string {
  const code = (error as NodeJS.ErrnoException)?.code
  if (code === 'ENOENT') return '文件不存在或已经移动。'
  if (code === 'EACCES' || code === 'EPERM') return '没有读取此文件的权限。'
  if (code === 'ELOOP') return '不支持符号链接，请选择原始文件。'
  if (code) return '无法读取此文件，请确认它是可访问的本地文本文件。'
  return error instanceof Error ? error.message : '无法读取此文件。'
}

export async function importFilePaths(value: unknown): Promise<FileImportResult> {
  if (!Array.isArray(value) || value.length > MAX_REFERENCE_FILES)
    throw new Error('每次最多添加 8 个文件。')
  const files: TextFileReference[] = []
  const errors: string[] = []
  const seen = new Set<string>()
  let total = 0
  for (const path of value) {
    if (
      typeof path !== 'string' ||
      !isAbsolute(path) ||
      /[\u0000-\u001f\u007f-\u009f]/.test(path) ||
      [...path].length > 4096
    ) {
      errors.push('无法引用此文件，请从文件管理器重新拖入。')
      continue
    }
    try {
      const file = await readReference(path)
      if (seen.has(file.path!)) continue
      seen.add(file.path!)
      const bytes = Buffer.byteLength(file.text, 'utf8')
      if (total + bytes > MAX_REFERENCE_TOTAL_BYTES)
        throw new Error('引用文件合计不能超过 512 KiB，请分批添加。')
      total += bytes
      files.push(file)
    } catch (error) {
      errors.push(`${basename(path)}：${importError(error)}`)
    }
  }
  return { files, errors }
}
