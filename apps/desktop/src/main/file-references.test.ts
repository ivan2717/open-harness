import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { clipboardFilePaths, importClipboardFiles, importFilePaths } from './file-references'
import { MAX_FILE_REFERENCE_BYTES as MAX_REFERENCE_BYTES } from '../shared/file-references'

describe('explicit local file references', () => {
  let directory: string
  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'openharness-files-'))
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  it('takes UTF-8 snapshots in selection order and deduplicates the same path', async () => {
    const first = join(directory, '中文 notes.md')
    const second = join(directory, 'empty.txt')
    await writeFile(first, '你好 👋\n\tconst n = 1\n')
    await writeFile(second, '')
    const result = await importFilePaths([first, second, first])
    expect(result.errors).toEqual([])
    expect(result.files.map((file) => [file.name, file.text])).toEqual([
      ['中文 notes.md', '你好 👋\n\tconst n = 1\n'],
      ['empty.txt', ''],
    ])
    await writeFile(first, 'changed later')
    expect(result.files[0].text).toBe('你好 👋\n\tconst n = 1\n')
  })

  it('returns per-file failures while retaining other readable files', async () => {
    const good = join(directory, 'readme.txt')
    const folder = join(directory, 'folder')
    const link = join(directory, 'alias.txt')
    const binary = join(directory, 'binary.txt')
    const invalid = join(directory, 'invalid.txt')
    await writeFile(good, 'Keep this reference')
    await mkdir(folder)
    await symlink(good, link)
    await writeFile(binary, 'header\0binary')
    await writeFile(invalid, Buffer.from([0xc3, 0x28]))
    const result = await importFilePaths([
      good,
      folder,
      link,
      binary,
      invalid,
      join(directory, 'missing.txt'),
    ])
    expect(result.files).toHaveLength(1)
    expect(result.errors).toHaveLength(5)
    expect(result.errors.join('\n')).toContain('普通文件')
    expect(result.errors.join('\n')).toContain('二进制')
    expect(result.errors.join('\n')).toContain('UTF-8')
    expect(result.errors.join('\n')).toContain('不存在')
  })

  it('rejects PDF and image formats even if their bytes happen to be valid text', async () => {
    const pdf = join(directory, 'notes.PDF')
    const image = join(directory, 'photo.png')
    const hiddenPdf = join(directory, 'no-extension')
    for (const path of [pdf, image]) await writeFile(path, 'text')
    await writeFile(hiddenPdf, '%PDF-1.7')
    const result = await importFilePaths([pdf, image, hiddenPdf])
    expect(result.files).toEqual([])
    expect(result.errors).toHaveLength(3)
  })

  it('preserves UTF-8 BOM and rejects non-text control characters', async () => {
    const bom = join(directory, 'bom.txt')
    const formFeed = join(directory, 'formfeed.txt')
    const c1 = join(directory, 'c1.txt')
    await writeFile(bom, '\ufefftext\twith\r\nnewlines')
    await writeFile(formFeed, 'text\ftext')
    await writeFile(c1, 'text\u0085text')
    const result = await importFilePaths([bom, formFeed, c1])
    expect(result.files.map((file) => file.text)).toEqual(['\ufefftext\twith\r\nnewlines'])
    expect(result.errors).toHaveLength(2)
  })

  it('enforces file and total byte limits without truncating content', async () => {
    const paths = [0, 1, 2, 3].map((index) => join(directory, `${index}.txt`))
    for (const path of paths.slice(0, 3)) await writeFile(path, 'a'.repeat(MAX_REFERENCE_BYTES))
    await writeFile(paths[3], 'a'.repeat(MAX_REFERENCE_BYTES + 1))
    const result = await importFilePaths(paths)
    expect(result.files).toHaveLength(2)
    expect(result.files.every((file) => file.text.length === MAX_REFERENCE_BYTES)).toBe(true)
    expect(result.errors[0]).toContain('512 KiB')
    expect(result.errors[1]).toContain('256 KiB')
    await expect(importFilePaths(Array(9).fill(paths[0]))).rejects.toThrow('8')
    expect((await importFilePaths(['relative.txt', '/tmp/file\0name'])).errors).toHaveLength(2)
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

  it('reads only the file URI representation, preserving ordinary clipboard text', async () => {
    const path = join(directory, 'clipboard.txt')
    await writeFile(path, 'clipboard snapshot')
    const requested: string[] = []
    const result = await importClipboardFiles(async () => [
      {
        types: ['text/plain', 'text/uri-list'],
        getType: async (type) => {
          requested.push(type)
          return new Blob([pathToFileURL(path).href])
        },
      },
    ])
    expect(requested).toEqual(['text/uri-list'])
    expect(result.files[0].text).toBe('clipboard snapshot')
    expect(
      await importClipboardFiles(async () => [
        {
          types: ['text/plain'],
          getType: async () => {
            throw new Error('ordinary text must not be read')
          },
        },
      ]),
    ).toEqual({ files: [], errors: [] })
  })

  it('turns clipboard read failures and oversized representations into useful errors', async () => {
    expect(
      (
        await importClipboardFiles(async () => {
          throw new Error('剪贴板读取失败。')
        })
      ).errors,
    ).toEqual(['剪贴板读取失败。'])
    const result = await importClipboardFiles(async () => [
      {
        types: ['text/uri-list'],
        getType: async () => new Blob(['#'.repeat(64 * 1024 + 1)]),
      },
    ])
    expect(result.files).toEqual([])
    expect(result.errors[0]).toContain('过大')
  })
})
