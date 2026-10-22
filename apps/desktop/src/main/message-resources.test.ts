import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { RunDetail } from '../shared/contracts'
import {
  byteRange,
  externalUrl,
  MessageResourceStore,
  resourceReference,
} from './message-resources'

describe('message presentation resources', () => {
  let directory: string
  let workspace: string
  let cache: string
  let run: RunDetail
  let store: MessageResourceStore
  let originalInput: string

  const reference = (source: string) => ({ runId: 'run-1', messageId: 'event-2', source })
  const backend = {
    request: async ({ path }: { path: string; method: 'GET' }): Promise<unknown> => {
      if (path === '/api/runs/run-1') return run
      if (path === '/api/runs/run-1/messages/event-0/files/0')
        return { name: 'input.md', path: join(directory, 'input.md'), text: originalInput }
      throw new Error('unknown request')
    },
  }

  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'openharness-resources-'))
    workspace = join(directory, 'project')
    cache = join(directory, 'cache')
    await mkdir(workspace)
    originalInput = '# Original input\n'
    run = {
      id: 'run-1',
      workspace_path: workspace,
      messages: [
        {
          id: 'event-0',
          role: 'user',
          text: 'Read it',
          files: [{ name: 'input.md', size: 17 }],
          status: 'complete',
        },
        { id: 'event-2', role: 'assistant', text: '', status: 'complete' },
      ],
    } as RunDetail
    store = new MessageResourceStore(cache, backend)
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  async function mention(source: string, content: string | Buffer, name = source) {
    await writeFile(join(workspace, name), content)
    run.messages[1].text += `[file](<${source}>)\n`
    return store.resolve(reference(source))
  }

  it('persists a named snapshot across original edits, deletion and process restart', async () => {
    const first = await mention('计划 sample.md', '# Version one')
    expect(first.kind).toBe('text')
    expect(first.text).toBe('# Version one')
    expect(first.canOpen).toBe(true)
    await writeFile(join(workspace, '计划 sample.md'), '# Version two')
    expect((await store.resolve(reference('计划 sample.md'))).text).toBe('# Version one')
    await rm(join(workspace, '计划 sample.md'))
    store = new MessageResourceStore(cache, backend)
    const restored = await store.resolve(reference('计划 sample.md'))
    expect(restored).toEqual(first)
    expect(await readFile(store.archivePath(restored.id), 'utf8')).toBe('# Version one')
  })

  it('previews input snapshots without reading the current source file', async () => {
    await writeFile(join(directory, 'input.md'), 'changed source')
    const value = await store.resolve({ runId: 'run-1', messageId: 'event-0', fileIndex: 0 })
    expect(value.text).toBe(originalInput)
    await expect(
      store.resolve({ runId: 'run-1', messageId: 'event-2', fileIndex: 0 }),
    ).rejects.toThrow('不存在')
    await expect(
      store.resolve({ runId: 'run-1', messageId: 'event-0', fileIndex: 1 }),
    ).rejects.toThrow('不存在')
  })

  it('contains paths in the selected workspace and rejects escaping aliases', async () => {
    const outside = join(directory, 'private.txt')
    await writeFile(outside, 'private')
    await symlink(outside, join(workspace, 'alias.txt'))
    run.messages[1].text = [
      '../private.txt',
      outside,
      'alias.txt',
      '%2E%2E/private.txt',
      'file://' + outside,
    ]
      .map((source) => `[file](<${source}>)`)
      .join('\n')
    for (const source of [
      '../private.txt',
      outside,
      'alias.txt',
      '%2E%2E/private.txt',
      'file://' + outside,
    ])
      await expect(store.resolve(reference(source))).rejects.toThrow(/项目/)
    await mkdir(join(workspace, 'folder'))
    run.messages[1].text += '\n[folder](folder)'
    await expect(store.resolve(reference('folder'))).rejects.toThrow('普通文件')
  })

  it('requires a settled assistant message with the requested target', async () => {
    await writeFile(join(workspace, 'report.txt'), 'report')
    await expect(store.resolve(reference('report.txt'))).rejects.toThrow('未关联')
    run.messages[1].text = '[report](report.txt)'
    run.messages[1].status = 'streaming'
    await expect(store.resolve(reference('report.txt'))).rejects.toThrow('等待')
    run.messages[1].status = 'complete'
    run.workspace_path = undefined
    await expect(store.resolve(reference('report.txt'))).rejects.toThrow('未关联项目')
  })

  it('handles encoded names and source-code line anchors without changing file access', async () => {
    const file = await mention('hello%20world.ts:12:3', 'export const a = 1', 'hello world.ts')
    expect(file.name).toBe('hello world.ts')
    expect(file.text).toContain('export')
    expect(file.canOpen).toBe(false)
    const second = await mention('hello%20world.ts#L12', 'export const a = 2', 'hello world.ts')
    expect(second.text).toContain('a = 2')
    const bare = await mention('app.ts:12:3', 'export const b = 1', 'app.ts')
    expect(bare.name).toBe('app.ts')
    expect(bare.text).toContain('b = 1')
  })

  it('admits parsed escaped, entity and reference links, excluding code and unused definitions', async () => {
    const names = [
      'a(b).txt',
      'a&b.txt',
      'report sample.txt',
      'pic.png',
      'unused.txt',
      'secret.txt',
      'code.txt',
    ]
    for (const name of names) await writeFile(join(workspace, name), name)
    run.messages[1].text = [
      String.raw`[escaped](./a\(b\).txt)`,
      '[entity](./a&amp;b.txt)',
      '[reference][Report]',
      '![picture][image]',
      '',
      '[report]: <./report sample.txt>',
      '[image]: ./pic.png',
      '[report]: ./unused.txt',
      '[unused]: ./secret.txt',
      '',
      '`[fake](./code.txt)`',
    ].join('\n')
    for (const source of ['./a(b).txt', './a&b.txt', './report%20sample.txt', './pic.png'])
      expect((await store.resolve(reference(source))).size).toBeGreaterThan(0)
    for (const source of ['./unused.txt', './secret.txt', './code.txt'])
      await expect(store.resolve(reference(source))).rejects.toThrow('未关联')
  })

  it('never executes active file types and keeps metadata.json as a normal filename', async () => {
    for (const name of ['page.html', 'script.js', 'install.sh']) {
      const value = await mention(name, '<script>unexpected()</script>')
      expect(value.kind).toBe('text')
      expect(value.canOpen).toBe(false)
      const response = await store.response(new Request(value.url))
      expect(response.headers.get('content-type')).toBe('text/plain; charset=utf-8')
      expect(response.headers.get('content-security-policy')).toContain("default-src 'none'")
    }
    const metadata = await mention('metadata.json', '{"valid":true}')
    store = new MessageResourceStore(cache, backend)
    expect((await store.resolve(reference('metadata.json'))).text).toBe('{"valid":true}')
    expect(metadata.name).toBe('metadata.json')
  })

  it('serves SVG only for inert image previews, with scripts and external loads disabled', async () => {
    const resource = await mention(
      'diagram.svg',
      '<svg xmlns="http://www.w3.org/2000/svg"><script>unexpected()</script></svg>',
    )
    expect(resource.kind).toBe('image')
    expect(resource.canOpen).toBe(false)
    const response = await store.response(new Request(resource.url))
    expect(response.headers.get('content-type')).toBe('image/svg+xml')
    expect(response.headers.get('content-security-policy')).toBe("default-src 'none'; sandbox")
    expect(response.headers.get('x-content-type-options')).toBe('nosniff')
    await response.body!.cancel()
  })

  it('serves only admitted opaque IDs with media MIME, HEAD and bounded byte ranges', async () => {
    const resource = await mention('demo.mp4', Buffer.from([0, 1, 2, 3, 4, 5]))
    const headers = { range: 'bytes=2-4' }
    const response = await store.response(new Request(resource.url, { headers }))
    expect(response.status).toBe(206)
    expect(response.headers.get('content-type')).toBe('video/mp4')
    expect(response.headers.get('content-range')).toBe('bytes 2-4/6')
    expect([...new Uint8Array(await response.arrayBuffer())]).toEqual([2, 3, 4])
    const head = await store.response(new Request(resource.url, { method: 'HEAD' }))
    expect(head.headers.get('content-length')).toBe('6')
    expect(await head.text()).toBe('')
    expect(
      (await store.response(new Request(resource.url, { headers: { range: 'bytes=20-40' } })))
        .status,
    ).toBe(416)
    expect((await store.response(new Request(resource.url, { method: 'POST' }))).status).toBe(405)
    store = new MessageResourceStore(cache, backend)
    expect((await store.response(new Request(resource.url))).status).toBe(404)
    await store.resolve(reference('demo.mp4'))
    expect((await store.response(new Request(resource.url))).status).toBe(200)
  })

  it('streams large ranges in bounded chunks and supports cancelling playback', async () => {
    const source = Buffer.alloc(256 * 1024, 7)
    const resource = await mention('long.webm', source)
    const response = await store.response(new Request(resource.url))
    const reader = response.body!.getReader()
    const first = await reader.read()
    expect(first.value?.byteLength).toBe(64 * 1024)
    expect(first.value?.[0]).toBe(7)
    await reader.cancel()
    expect((await reader.read()).done).toBe(true)

    const ranged = await store.response(
      new Request(resource.url, { headers: { range: 'bytes=65530-65550' } }),
    )
    expect(new Uint8Array(await ranged.arrayBuffer())).toEqual(new Uint8Array(21).fill(7))
  })

  it('validates URI schemes and range syntax', () => {
    for (const source of [
      'javascript:alert(1)',
      'file:///tmp/secret',
      'data:text/html,hello',
      'https://user:pass@example.com',
      'https://example.com\n',
    ])
      expect(() => externalUrl(source)).toThrow()
    expect(externalUrl('https://example.com/a')).toBe('https://example.com/a')
    expect(externalUrl('mailto:user@example.com')).toBe('mailto:user@example.com')
    expect(byteRange('bytes=-3', 6)).toEqual({ start: 3, end: 5 })
    expect(byteRange('bytes=2-', 6)).toEqual({ start: 2, end: 5 })
    for (const range of [
      'bytes=0-1,3-4',
      'bytes=-0',
      'bytes=2-1',
      'bytes=9007199254740992-',
      'items=0-1',
    ])
      expect(() => byteRange(range, 6)).toThrow()
    expect(() => resourceReference({ runId: '../private', messageId: 'm', source: 'x' })).toThrow()
    expect(() =>
      resourceReference({ runId: 'r', messageId: 'm', source: 'x', fileIndex: 0 }),
    ).toThrow()
  })
})
