import { constants } from 'node:fs'
import { createHash, randomUUID } from 'node:crypto'
import { lstat, mkdir, open, realpath, rename, rm, writeFile } from 'node:fs/promises'
import { basename, extname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fromMarkdown } from 'mdast-util-from-markdown'
import type { Nodes } from 'mdast'
import type { RunDetail, TextFileReference } from '../shared/contracts'
import type { ResolvedResource, ResourceKind, ResourceReference } from '../shared/message-resources'
import { isFileLocation } from '../shared/message-resources'

export const RESOURCE_SCHEME = 'openharness-resource'
export const MAX_RESOURCE_BYTES = 100 * 1024 * 1024
const MAX_TEXT_PREVIEW_BYTES = 256 * 1024
const RESOURCE_READ_CHUNK_BYTES = 64 * 1024
const idPattern = /^[a-f0-9]{64}$/

interface ResourceBackend {
  request(request: { path: string; method: 'GET' }): Promise<unknown>
}

interface ResourceRecord extends Omit<ResolvedResource, 'url' | 'text'> {
  reference: ResourceReference
}

const mediaTypes: Record<string, [ResourceKind, string]> = {
  '.png': ['image', 'image/png'],
  '.jpg': ['image', 'image/jpeg'],
  '.jpeg': ['image', 'image/jpeg'],
  '.gif': ['image', 'image/gif'],
  '.webp': ['image', 'image/webp'],
  '.avif': ['image', 'image/avif'],
  '.bmp': ['image', 'image/bmp'],
  // SVG is only embedded as an image. The response CSP prevents active document use.
  '.svg': ['image', 'image/svg+xml'],
  '.mp4': ['video', 'video/mp4'],
  '.m4v': ['video', 'video/mp4'],
  '.webm': ['video', 'video/webm'],
  '.mov': ['video', 'video/quicktime'],
  '.mp3': ['audio', 'audio/mpeg'],
  '.wav': ['audio', 'audio/wav'],
  '.ogg': ['audio', 'audio/ogg'],
  '.oga': ['audio', 'audio/ogg'],
  '.m4a': ['audio', 'audio/mp4'],
  '.flac': ['audio', 'audio/flac'],
}
const documentTypes: Record<string, string> = {
  '.pdf': 'application/pdf',
  '.docx': 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  '.xlsx': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  '.pptx': 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
}
const safeOpenText = new Set(['.txt', '.md', '.markdown', '.csv', '.tsv', '.json', '.log'])

function validName(name: unknown): name is string {
  return (
    typeof name === 'string' &&
    Boolean(name.trim()) &&
    name !== '.' &&
    name !== '..' &&
    name.length <= 255 &&
    !/[\/\\\u0000-\u001f\u007f-\u009f]/.test(name)
  )
}

function identifier(value: unknown): string {
  if (typeof value !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(value))
    throw new Error('文件所属会话无效。')
  return value
}

function readableError(error: unknown): Error {
  const code = (error as NodeJS.ErrnoException)?.code
  if (code === 'ENOENT') return new Error('文件不存在或已被移动。')
  if (code === 'EACCES' || code === 'EPERM') return new Error('无法访问此文件，请检查访问权限。')
  if (['ELOOP', 'ENOTDIR', 'EISDIR'].includes(code ?? ''))
    return new Error('文件路径无效，请确认它指向一个普通文件。')
  if (code === 'ENAMETOOLONG') return new Error('文件路径过长。')
  if (error instanceof URIError) return new Error('文件地址无效。')
  return error instanceof Error ? error : new Error('无法读取此文件。')
}

export function resourceReference(value: unknown): ResourceReference {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('文件引用无效。')
  const item = value as Record<string, unknown>
  const runId = identifier(item.runId)
  const messageId = identifier(item.messageId)
  if (item.source !== undefined) {
    if (
      item.fileIndex !== undefined ||
      typeof item.source !== 'string' ||
      !item.source.trim() ||
      item.source.length > 8192 ||
      /[\u0000-\u001f\u007f-\u009f]/.test(item.source)
    )
      throw new Error('文件地址无效。')
    return { runId, messageId, source: item.source }
  }
  if (
    !Number.isSafeInteger(item.fileIndex) ||
    Number(item.fileIndex) < 0 ||
    Number(item.fileIndex) > 7
  )
    throw new Error('引用文件序号无效。')
  return { runId, messageId, fileIndex: Number(item.fileIndex) }
}

export function externalUrl(value: unknown): string {
  if (typeof value !== 'string' || value.length > 8192 || /[\u0000-\u0020\u007f]/.test(value))
    throw new Error('链接地址无效。')
  let url: URL
  try {
    url = new URL(value)
  } catch {
    throw new Error('链接地址无效。')
  }
  if (!['http:', 'https:', 'mailto:'].includes(url.protocol) || url.username || url.password)
    throw new Error('不支持打开此链接类型。')
  return url.href
}

function within(root: string, path: string): boolean {
  const suffix = relative(root, path)
  return suffix !== '' && suffix !== '..' && !suffix.startsWith(`..${sep}`) && !isAbsolute(suffix)
}

/** Markdown file links may carry a source-code location; it is not part of the filename. */
function sourcePath(source: string): string {
  let path: string
  if (/^file:/i.test(source)) {
    const url = new URL(source)
    if (url.hostname && url.hostname !== 'localhost') throw new Error('仅支持本地文件。')
    if (url.search || (url.hash && !/^#L?\d+(?:[-:]L?\d+)?$/.test(url.hash)))
      throw new Error('文件地址无效。')
    url.hash = ''
    path = fileURLToPath(url)
  } else {
    path = decodeURIComponent(source.replace(/#L?\d+(?:[-:]L?\d+)?$/, ''))
    if (/^[a-z][a-z0-9+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path) && !isFileLocation(path))
      throw new Error('仅支持项目内的本地文件。')
  }
  path = path.replace(/:\d+(?::\d+)?$/, '')
  if (!path || /[\u0000-\u001f\u007f-\u009f]/.test(path)) throw new Error('文件地址无效。')
  return path
}

function textPreview(bytes: Uint8Array): string | undefined {
  if (bytes.length > MAX_TEXT_PREVIEW_BYTES) return undefined
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    return /[\u0000-\u0008\u000b\u000e-\u001f\u007f]/.test(text) ? undefined : text
  } catch {
    return undefined
  }
}

function normalizedDestination(source: string): string {
  try {
    return decodeURIComponent(source)
  } catch {
    return source
  }
}

/** Match the Markdown parser's decoded destinations, including reference-style links. */
function markdownDestinations(text: string): Set<string> {
  const tree = fromMarkdown(text)
  const definitions = new Map<string, string>()
  const walk = (node: Nodes, visit: (node: Nodes) => void): void => {
    visit(node)
    if ('children' in node) for (const child of node.children) walk(child, visit)
  }
  walk(tree, (node) => {
    // CommonMark uses the first definition for a duplicated label.
    if (node.type === 'definition' && !definitions.has(node.identifier))
      definitions.set(node.identifier, node.url)
  })
  const destinations = new Set<string>()
  walk(tree, (node) => {
    let source: string | undefined
    if (node.type === 'link' || node.type === 'image') source = node.url
    if (node.type === 'linkReference' || node.type === 'imageReference')
      source = definitions.get(node.identifier)
    if (source !== undefined) destinations.add(normalizedDestination(source))
  })
  return destinations
}

function describe(
  name: string,
  bytes: Uint8Array,
): Pick<ResourceRecord, 'kind' | 'mimeType' | 'canOpen'> {
  const extension = extname(name).toLowerCase()
  const media = mediaTypes[extension]
  if (media) return { kind: media[0], mimeType: media[1], canOpen: extension !== '.svg' }
  if (documentTypes[extension])
    return { kind: 'file', mimeType: documentTypes[extension], canOpen: true }
  if (textPreview(bytes) !== undefined)
    return { kind: 'text', mimeType: 'text/plain', canOpen: safeOpenText.has(extension) }
  return { kind: 'file', mimeType: 'application/octet-stream', canOpen: false }
}

async function readBounded(path: string, maximum: number): Promise<Buffer> {
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const metadata = await handle.stat()
    if (!metadata.isFile()) throw new Error('仅支持普通文件。')
    if (metadata.size > maximum) throw new Error('文件超过 100 MiB，无法在会话中保存预览。')
    const buffer = Buffer.alloc(metadata.size + 1)
    let count = 0
    while (count < buffer.length) {
      const result = await handle.read(buffer, count, buffer.length - count, count)
      if (!result.bytesRead) break
      count += result.bytesRead
    }
    if (count > metadata.size) throw new Error('文件正在变化，请稍后重试。')
    const after = await lstat(path)
    if (
      !after.isFile() ||
      after.dev !== metadata.dev ||
      after.ino !== metadata.ino ||
      after.size !== metadata.size ||
      after.mtimeMs !== metadata.mtimeMs ||
      after.ctimeMs !== metadata.ctimeMs
    )
      throw new Error('文件正在变化，请稍后重试。')
    return buffer.subarray(0, count)
  } finally {
    await handle.close()
  }
}

export function byteRange(
  header: string | null,
  size: number,
): { start: number; end: number } | null {
  if (header === null) return null
  const match = /^bytes=(\d*)-(\d*)$/.exec(header)
  if (!match || (!match[1] && !match[2]) || size === 0) throw new Error('Invalid range')
  const start = match[1] ? Number(match[1]) : Math.max(0, size - Number(match[2]))
  const end = match[1] && match[2] ? Math.min(Number(match[2]), size - 1) : size - 1
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= size || end < start)
    throw new Error('Invalid range')
  return { start, end }
}

/** Owns immutable presentation snapshots, independent from model context and provider protocols. */
export class MessageResourceStore {
  private records = new Map<string, ResourceRecord>()
  private pending = new Map<string, Promise<ResolvedResource>>()
  private queue = Promise.resolve()
  private messageLinks = new Map<string, { text: string; destinations: Set<string> }>()

  constructor(
    private directory: string,
    private backend: ResourceBackend,
  ) {}

  async resolve(value: unknown): Promise<ResolvedResource> {
    const reference = resourceReference(value)
    const id = createHash('sha256').update(JSON.stringify(reference)).digest('hex')
    const existing = this.pending.get(id)
    if (existing) return existing
    // Bound peak memory when a completed response contains many large resources.
    const task = this.queue
      .then(() => this.resolveReference(id, reference))
      .catch((error) => {
        throw readableError(error)
      })
    this.queue = task.then(
      () => undefined,
      () => undefined,
    )
    this.pending.set(id, task)
    try {
      return await task
    } finally {
      this.pending.delete(id)
    }
  }

  private async resolveReference(
    id: string,
    reference: ResourceReference,
  ): Promise<ResolvedResource> {
    const run = (await this.backend.request({
      path: `/api/runs/${reference.runId}`,
      method: 'GET',
    })) as RunDetail
    const message = run.messages.find((message) => message.id === reference.messageId)
    if (!message) throw new Error('消息不存在。')
    if ('source' in reference) {
      if (message.role !== 'assistant' || message.status !== 'complete')
        throw new Error('请等待回答完成后查看文件。')
      const key = `${reference.runId}:${reference.messageId}`
      let links = this.messageLinks.get(key)
      if (!links || links.text !== message.text) {
        links = { text: message.text, destinations: markdownDestinations(message.text) }
        this.messageLinks.delete(key)
        // Bound retention while avoiding another parse for every link in the same response.
        if (this.messageLinks.size >= 32)
          this.messageLinks.delete(this.messageLinks.keys().next().value!)
        this.messageLinks.set(key, links)
      }
      if (!links.destinations.has(normalizedDestination(reference.source!)))
        throw new Error('此文件未关联到当前消息。')
    } else if (message.role !== 'user' || !message.files?.[reference.fileIndex]) {
      throw new Error('引用文件不存在。')
    }
    const cached = await this.load(id, reference)
    if (cached) return this.present(cached)

    let name: string
    let bytes: Buffer
    let path: string | undefined
    if ('source' in reference) {
      if (!run.workspace_path) throw new Error('此任务未关联项目，无法访问本地输出文件。')
      const root = await realpath(run.workspace_path)
      const candidate = resolve(root, sourcePath(reference.source!))
      if (!within(root, candidate)) throw new Error('只能查看当前项目中的文件。')
      path = await realpath(candidate)
      if (!within(root, path)) throw new Error('文件指向项目之外，无法预览。')
      name = basename(path)
      bytes = await readBounded(path, MAX_RESOURCE_BYTES)
      if ((await realpath(candidate)) !== path || (await realpath(run.workspace_path)) !== root)
        throw new Error('文件路径已变化，请重试。')
    } else {
      const file = (await this.backend.request({
        path: `/api/runs/${reference.runId}/messages/${reference.messageId}/files/${reference.fileIndex}`,
        method: 'GET',
      })) as TextFileReference
      name = file.name
      bytes = Buffer.from(file.text, 'utf8')
      path = file.path
      if (bytes.length > MAX_TEXT_PREVIEW_BYTES) throw new Error('引用文件过大。')
    }
    if (!validName(name)) throw new Error('文件名无效。')
    const record: ResourceRecord = {
      id,
      name,
      size: bytes.length,
      ...describe(name, bytes),
      reference,
      ...(path ? { sourcePath: path } : {}),
    }
    const folder = join(this.directory, id)
    await mkdir(join(folder, 'file'), { recursive: true, mode: 0o700 })
    const temporary = join(folder, `.${randomUUID()}.tmp`)
    try {
      await writeFile(temporary, bytes, { flag: 'wx', mode: 0o600 })
      await rename(temporary, join(folder, 'file', name))
      await writeFile(`${temporary}.json`, JSON.stringify(record), { flag: 'wx', mode: 0o600 })
      await rename(`${temporary}.json`, join(folder, 'metadata.json'))
    } finally {
      await rm(temporary, { force: true })
      await rm(`${temporary}.json`, { force: true })
    }
    this.records.set(id, record)
    return this.present(record, bytes)
  }

  private async load(
    id: string,
    reference: ResourceReference,
  ): Promise<ResourceRecord | undefined> {
    const active = this.records.get(id)
    if (active) return active
    let metadata: Buffer
    try {
      metadata = await readBounded(join(this.directory, id, 'metadata.json'), 32 * 1024)
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
      throw error
    }
    const record = JSON.parse(metadata.toString()) as ResourceRecord
    if (
      record.id !== id ||
      JSON.stringify(record.reference) !== JSON.stringify(reference) ||
      !validName(record.name)
    )
      throw new Error('文件快照记录无效。')
    // A committed snapshot with missing bytes must not silently change to the current source.
    const bytes = await readBounded(
      join(this.directory, id, 'file', record.name),
      MAX_RESOURCE_BYTES,
    )
    if (bytes.length !== record.size) throw new Error('文件快照不完整。')
    Object.assign(record, describe(record.name, bytes))
    this.records.set(id, record)
    return record
  }

  private async present(record: ResourceRecord, bytes?: Buffer): Promise<ResolvedResource> {
    const { reference: _, ...result } = record
    if (record.kind === 'text')
      bytes ??= await readBounded(this.archivePath(record.id), MAX_TEXT_PREVIEW_BYTES)
    return {
      ...result,
      url: `${RESOURCE_SCHEME}://resource/${record.id}`,
      ...(bytes && record.kind === 'text' ? { text: textPreview(bytes) } : {}),
    }
  }

  /** Only IDs resolved for this window session can reach disk or system actions. */
  resource(id: unknown): ResourceRecord {
    if (typeof id !== 'string' || !idPattern.test(id)) throw new Error('文件引用无效。')
    const record = this.records.get(id)
    if (!record) throw new Error('请先打开消息中的文件预览。')
    return record
  }

  archivePath(id: unknown): string {
    const record = this.resource(id)
    return join(this.directory, record.id, 'file', record.name)
  }

  async response(request: Request): Promise<Response> {
    const url = new URL(request.url)
    if (
      url.protocol !== `${RESOURCE_SCHEME}:` ||
      url.hostname !== 'resource' ||
      url.search ||
      url.hash
    )
      return new Response(null, { status: 404 })
    if (!['GET', 'HEAD'].includes(request.method)) return new Response(null, { status: 405 })
    let record: ResourceRecord
    try {
      record = this.resource(url.pathname.slice(1))
    } catch {
      return new Response(null, { status: 404 })
    }
    const headers = new Headers({
      'Content-Type': ['image', 'video', 'audio'].includes(record.kind)
        ? record.mimeType
        : 'text/plain; charset=utf-8',
      'X-Content-Type-Options': 'nosniff',
      'Content-Security-Policy': "default-src 'none'; sandbox",
      'Accept-Ranges': 'bytes',
      'Cache-Control': 'private, max-age=31536000, immutable',
      'Access-Control-Allow-Origin': '*',
    })
    let range: ReturnType<typeof byteRange>
    try {
      range = byteRange(request.headers.get('range'), record.size)
    } catch {
      headers.set('Content-Range', `bytes */${record.size}`)
      return new Response(null, { status: 416, headers })
    }
    const start = range?.start ?? 0
    const length = range ? range.end - range.start + 1 : record.size
    headers.set('Content-Length', String(length))
    if (range) headers.set('Content-Range', `bytes ${range.start}-${range.end}/${record.size}`)
    if (request.method === 'HEAD') return new Response(null, { status: range ? 206 : 200, headers })
    const handle = await open(
      this.archivePath(record.id),
      constants.O_RDONLY | constants.O_NOFOLLOW,
    )
    try {
      const stat = await handle.stat()
      if (!stat.isFile() || stat.size !== record.size) throw new Error('文件快照不完整。')
    } catch (error) {
      await handle.close()
      throw error
    }
    let position = start
    let cancelled = false
    let closed = false
    const close = async (): Promise<void> => {
      if (closed) return
      closed = true
      await handle.close()
    }
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          if (position < start + length) {
            const bytes = new Uint8Array(
              Math.min(RESOURCE_READ_CHUNK_BYTES, start + length - position),
            )
            const { bytesRead } = await handle.read(bytes, 0, bytes.length, position)
            if (cancelled) return
            if (!bytesRead) throw new Error('文件快照不完整。')
            position += bytesRead
            controller.enqueue(bytes.subarray(0, bytesRead))
          }
          if (position === start + length) {
            await close()
            if (!cancelled) controller.close()
          }
        } catch (error) {
          await close()
          if (!cancelled) controller.error(error)
        }
      },
      async cancel() {
        cancelled = true
        await close()
      },
    })
    return new Response(body, { status: range ? 206 : 200, headers })
  }
}
