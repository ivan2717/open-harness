import { createHash } from 'node:crypto'
import { mkdtemp, mkdir, rm, symlink, truncate, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { nativeImage } from 'electron'
import { clipboardFilePaths, importClipboardFiles, importFilePaths } from './file-references'
import { MAX_ATTACHMENT_SOURCE_BYTES, MAX_REFERENCE_FILES } from '../shared/file-references'

vi.mock('electron', () => ({ nativeImage: { createFromBuffer: vi.fn() } }))

function capturedFiles() {
  const saved = new Map<string, Buffer>()
  const store = vi.fn(async (name: string, bytes: Buffer) => {
    const id = createHash('sha256').update(bytes).digest('hex')
    saved.set(id, Buffer.from(bytes))
    return { name, attachment: { id, size: bytes.length } }
  })
  return { saved, store }
}

describe('explicit local file references', () => {
  let directory: string
  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'openharness-files-'))
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  it('retains original bytes and sends only metadata, independently of later source edits', async () => {
    const { saved, store } = capturedFiles()
    const first = join(directory, '中文 notes.md')
    const second = join(directory, 'empty.txt')
    const text = '你好 👋\n'.repeat(100_000)
    await writeFile(first, text)
    await writeFile(second, '')
    const result = await importFilePaths([first, second, first], store)
    expect(result.errors).toEqual([])
    expect(result.files.map((file) => file.name)).toEqual(['中文 notes.md', 'empty.txt'])
    expect(store).toHaveBeenCalledTimes(2)
    expect(JSON.stringify(result.files).length).toBeLessThan(1024)
    expect(result.files[0]).not.toHaveProperty('text')
    expect(result.files[0].attachment?.size).toBe(Buffer.byteLength(text))
    await writeFile(first, 'changed later')
    expect(saved.get(result.files[0].attachment!.id)?.toString()).toBe(text)
  })

  it('captures PDF, Office and arbitrary binary originals without attempting text extraction', async () => {
    const { store, saved } = capturedFiles()
    const names = ['scan.pdf', 'report.docx', 'sheet.xlsx', 'slides.pptx', 'data.zip', 'legacy.dat']
    const bytes = Buffer.from([0, 255, 0xc3, 0x28])
    for (const name of names) await writeFile(join(directory, name), bytes)
    const result = await importFilePaths(
      names.map((name) => join(directory, name)),
      store,
    )
    expect(result.errors).toEqual([])
    expect(result.files).toHaveLength(names.length)
    expect(store).toHaveBeenCalledTimes(names.length)
    expect(saved.get(result.files[0].attachment!.id)).toEqual(bytes)
    expect(result.files.every((file) => !('text' in file))).toBe(true)
  })

  it('returns per-file errors while retaining readable files and refuses symlinks/directories', async () => {
    const { store } = capturedFiles()
    const good = join(directory, 'notes.txt')
    const folder = join(directory, 'folder')
    const link = join(directory, 'link.txt')
    await writeFile(good, 'ok')
    await mkdir(folder)
    await symlink(good, link)
    const result = await importFilePaths(
      [good, folder, link, join(directory, 'missing.txt')],
      store,
    )
    expect(result.files).toHaveLength(1)
    expect(result.errors).toHaveLength(3)
    expect(result.errors.join('\n')).toContain('普通文件')
    expect(result.errors.join('\n')).toContain('不存在')
    expect(store).toHaveBeenCalledOnce()
  })

  it('bounds original sources at 100 MiB without the old extracted-text or aggregate limits', async () => {
    const { store } = capturedFiles()
    const paths = [0, 1, 2].map((index) => join(directory, `${index}.txt`))
    for (const path of paths) await writeFile(path, 'a'.repeat(600 * 1024))
    expect((await importFilePaths(paths, store)).files).toHaveLength(3)
    await truncate(paths[0], MAX_ATTACHMENT_SOURCE_BYTES + 1)
    expect((await importFilePaths([paths[0]], store)).errors[0]).toContain('100 MiB')
    await expect(
      importFilePaths(Array(MAX_REFERENCE_FILES + 1).fill(paths[0]), store),
    ).rejects.toThrow('64')
    expect((await importFilePaths(['relative.txt', '/tmp/file\0name'], store)).errors).toHaveLength(
      2,
    )
  })

  it('surfaces a storage failure and retains other selections', async () => {
    const { store } = capturedFiles()
    const paths = ['first.pdf', 'second.txt'].map((name) => join(directory, name))
    for (const path of paths) await writeFile(path, 'fixture')
    store.mockRejectedValueOnce(new Error('保存失败'))
    const result = await importFilePaths(paths, store)
    expect(result.files.map((file) => file.name)).toEqual(['second.txt'])
    expect(result.errors).toEqual(['first.pdf：保存失败'])
    expect((await importFilePaths([paths[0]])).errors[0]).toContain('存储尚未就绪')
  })

  it('captures selected and native-pasted image files through the existing visual decoder', async () => {
    const png = Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq1sAAAAASUVORK5CYII=',
      'base64',
    )
    vi.mocked(nativeImage.createFromBuffer).mockReturnValue({
      isEmpty: () => false,
      getSize: () => ({ width: 1, height: 1 }),
      toPNG: () => png,
    } as never)
    const paths = Array.from({ length: 5 }, (_, index) => join(directory, `${index}.png`))
    for (const path of paths) await writeFile(path, png)
    const selected = await importFilePaths(paths)
    expect(selected.files).toHaveLength(4)
    expect(selected.errors[0]).toContain('4 张')
    expect(selected.files[0]).toMatchObject({
      name: '0.png',
      image: { mime_type: 'image/png', data_base64: png.toString('base64') },
    })
    const pasted = await importClipboardFiles(async () => [
      { types: ['text/uri-list'], getType: async () => new Blob([pathToFileURL(paths[0]).href]) },
    ])
    expect(pasted.files).toEqual([selected.files[0]])
    expect(pasted.errors).toEqual([])
  })

  it('matches clipboard identities before reading and skips image bytes captured by the paste event', async () => {
    const { store } = capturedFiles()
    const image = join(directory, 'screen.png')
    const document = join(directory, 'notes.pdf')
    await writeFile(image, 'image')
    await writeFile(document, 'PDF')
    const read = async () => [
      {
        types: ['text/uri-list'],
        getType: async () =>
          new Blob([[image, document].map((path) => pathToFileURL(path).href).join('\n')]),
      },
    ]
    const files = [
      { name: 'screen.png', size: 5, image: true },
      { name: 'notes.pdf', size: 3, image: false },
    ]
    const result = await importClipboardFiles(read, store, { files, skipImages: true })
    expect(result.errors).toEqual([])
    expect(result.files).toHaveLength(1)
    expect(result.files[0]).toMatchObject({ name: 'notes.pdf', attachment: { size: 3 } })
    expect(store).toHaveBeenCalledOnce()
    store.mockClear()
    for (const selectedFiles of [
      [{ ...files[0], name: 'other.png' }, files[1]],
      [files[0], { ...files[1], size: 4 }],
      [files[1]],
    ])
      expect(
        await importClipboardFiles(read, store, { files: selectedFiles, skipImages: true }),
      ).toEqual({ files: [], errors: [] })
    expect(store).not.toHaveBeenCalled()
    expect(
      (
        await importClipboardFiles(read, store, {
          files: [{ name: '../bad', size: -1 }],
          skipImages: true,
        })
      ).errors,
    ).toHaveLength(1)
  })

  it('decodes local URI lists including spaces and unicode, without network URLs', () => {
    const path = join(directory, '中文 # notes.txt')
    const uri = pathToFileURL(path).href
    expect(clipboardFilePaths(`# copied files\r\n${uri}\r\n${uri}\r\n`)).toEqual([path])
    for (const uri of [
      'https://example.com/file.txt',
      'file://remote/share.txt',
      'file:///tmp/a?x=1',
      'file:///tmp/a#fragment',
      'file:///tmp/a%00b',
      'file:///tmp/%2Fsecret',
      'not a URI',
    ])
      expect(() => clipboardFilePaths(uri)).toThrow()
    expect(() => clipboardFilePaths('#'.repeat(64 * 1024 + 1))).toThrow('过大')
  })

  it('reads only native file URLs, preserves ordinary clipboard text, and deduplicates selections', async () => {
    const { store } = capturedFiles()
    const path = join(directory, 'clipboard.txt')
    await writeFile(path, 'clipboard original')
    const requested: string[] = []
    const result = await importClipboardFiles(
      async () =>
        [path, path].map((path) => ({
          types: ['text/plain', 'text/uri-list'],
          getType: async (type) => {
            requested.push(type)
            return new Blob([pathToFileURL(path).href])
          },
        })),
      store,
    )
    expect(requested).toEqual(['text/uri-list', 'text/uri-list'])
    expect(result.files).toHaveLength(1)
    expect(store).toHaveBeenCalledWith('clipboard.txt', Buffer.from('clipboard original'))
    expect(
      await importClipboardFiles(
        async () => [
          {
            types: ['text/plain'],
            getType: async () => {
              throw new Error('must not read text')
            },
          },
        ],
        store,
      ),
    ).toEqual({ files: [], errors: [] })
  })

  it('enforces total count across clipboard items and provides useful recovery errors', async () => {
    const { store } = capturedFiles()
    const result = await importClipboardFiles(
      async () =>
        Array.from({ length: 65 }, (_, index) => ({
          types: ['text/uri-list'],
          getType: async () => new Blob([pathToFileURL(join(directory, `${index}.txt`)).href]),
        })),
      store,
    )
    expect(result).toEqual({ files: [], errors: ['每次最多添加 64 个文件。'] })
    expect((await importFilePaths([''], store)).errors).toEqual([
      '无法读取文件来源，请重新复制后粘贴，或使用“添加文件”。',
    ])
    expect(
      (
        await importClipboardFiles(async () => {
          throw new Error('剪贴板读取失败。')
        }, store)
      ).errors,
    ).toEqual(['剪贴板读取失败。'])
    expect(
      (
        await importClipboardFiles(
          async () => [
            {
              types: ['text/uri-list'],
              getType: async () => new Blob(['#'.repeat(64 * 1024 + 1)]),
            },
          ],
          store,
        )
      ).errors[0],
    ).toContain('过大')
  })
})
