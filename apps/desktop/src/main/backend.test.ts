import { createHash } from 'node:crypto'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import { resolve } from 'node:path'
import { Backend, backendSandboxEnvironment } from './backend'
import { apiRequest } from './protocol'
import { ModelRestoration } from './runtime-startup'

vi.mock('node:child_process', () => ({ spawn: vi.fn() }))

const fixtureBinary = resolve('fixture-api.exe')

describe('Windows sandbox helper location', () => {
  it.each([
    [
      'C:\\Users\\Example User\\AppData\\Local\\Programs\\Open Harness\\resources\\bin\\api.exe',
      'C:\\Users\\Example User\\AppData\\Local\\Programs\\Open Harness\\resources\\bin\\openharness-sandbox.exe',
    ],
    ['D:\\build cache\\debug\\api.exe', 'D:\\build cache\\debug\\openharness-sandbox.exe'],
  ])('uses the API sibling for packaged and development binaries (%s)', (binary, helper) => {
    expect(backendSandboxEnvironment(binary, 'win32')).toEqual({
      OPENHARNESS_WINDOWS_SANDBOX_HELPER: helper,
    })
  })

  it.each(['api.exe', 'C:api.exe', '\\api.exe'])(
    'rejects a Windows path that depends on the working directory (%s)',
    (binary) => {
      expect(() => backendSandboxEnvironment(binary, 'win32')).toThrow('must be absolute')
    },
  )

  it('does not configure the Windows helper on other platforms', () => {
    expect(
      backendSandboxEnvironment(
        '/Applications/Open Harness.app/Contents/Resources/bin/api',
        'darwin',
      ),
    ).toEqual({})
    expect(backendSandboxEnvironment('/app/bin/api', 'linux')).toEqual({})
  })
})

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  vi.useRealTimers()
  vi.mocked(spawn).mockReset()
})

function runtimeProcess() {
  const child = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout: new PassThrough(),
    stderr: new PassThrough(),
    kill: vi.fn(() => true),
    exitCode: null,
    signalCode: null,
  })
  vi.mocked(spawn).mockReturnValue(child as unknown as ReturnType<typeof spawn>)
  return child
}

function delayedBackend(responseDelay: number): Backend {
  vi.useFakeTimers()
  // Node's native AbortSignal timeout does not use the mocked JS timer queue.
  vi.spyOn(AbortSignal, 'timeout').mockImplementation((delay) => {
    const controller = new AbortController()
    setTimeout(() => controller.abort(), delay)
    return controller.signal
  })
  vi.stubGlobal(
    'fetch',
    vi.fn((_url: string, init: RequestInit) => {
      return new Promise<Response>((resolve, reject) => {
        const timer = setTimeout(() => {
          init.signal?.removeEventListener('abort', abort)
          resolve(new Response(JSON.stringify({ ready: true })))
        }, responseDelay)
        function abort(): void {
          clearTimeout(timer)
          reject(new DOMException('Aborted', 'AbortError'))
        }
        init.signal?.addEventListener('abort', abort, { once: true })
        if (init.signal?.aborted) abort()
      })
    }),
  )
  const backend = new Backend('unused-binary', 'unused-directory')
  vi.spyOn(backend, 'start').mockResolvedValue('http://127.0.0.1:1234')
  return backend
}

describe('local runtime request deadlines', () => {
  it('allows an MCP connection test to finish after the ordinary API deadline', async () => {
    const result = delayedBackend(45_000).request(apiRequest({ operation: 'testMcp', id: 'docs' }))
    await vi.advanceTimersByTimeAsync(45_000)
    await expect(result).resolves.toEqual({ ready: true })
  })

  it('keeps the 30-second deadline for ordinary API requests', async () => {
    const result = delayedBackend(45_000).request(apiRequest({ operation: 'extensionSettings' }))
    const assertion = expect(result).rejects.toThrow('Could not reach the local runtime')
    await vi.advanceTimersByTimeAsync(30_000)
    await assertion
  })

  it('still bounds MCP connection tests if the local runtime never responds', async () => {
    const result = delayedBackend(75_000).request(apiRequest({ operation: 'testMcp', id: 'docs' }))
    const assertion = expect(result).rejects.toThrow('Could not reach the local runtime')
    await vi.advanceTimersByTimeAsync(70_000)
    await assertion
  })
})

describe('local original attachment storage', () => {
  it('uploads unmodified bytes and validates the returned immutable reference', async () => {
    const backend = new Backend('unused', 'unused')
    vi.spyOn(backend, 'start').mockResolvedValue('http://127.0.0.1:1234')
    const bytes = Buffer.from('original PDF\0bytes')
    const file = {
      name: '中文 & notes.pdf',
      attachment: { id: createHash('sha256').update(bytes).digest('hex'), size: bytes.length },
    }
    const fetch = vi.fn(async () => new Response(JSON.stringify(file)))
    vi.stubGlobal('fetch', fetch)
    await expect(backend.importAttachment(file.name, bytes)).resolves.toEqual(file)
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
    expect(new URL(url).pathname).toBe('/api/attachments')
    expect(new URL(url).searchParams.get('name')).toBe(file.name)
    expect(init.body).toEqual(new Uint8Array(bytes))
    expect(init.headers).toMatchObject({
      Authorization: expect.stringMatching(/^Bearer [a-f0-9]+$/),
    })
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ ...file, attachment: { ...file.attachment, id: 'b'.repeat(64) } }),
          ),
      ),
    )
    await expect(backend.importAttachment(file.name, bytes)).rejects.toThrow('保存结果无效')
  })

  it('downloads only a bounded file from a saved message, never a caller-provided path', async () => {
    const backend = new Backend('unused', 'unused')
    vi.spyOn(backend, 'start').mockResolvedValue('http://127.0.0.1:1234')
    const bytes = Buffer.from('original\0bytes')
    const fetch = vi.fn(async (_url: string) => new Response(bytes))
    vi.stubGlobal('fetch', fetch)
    await expect(backend.attachmentContent('run-1', 'event-0', 12)).resolves.toEqual(bytes)
    expect(fetch.mock.calls[0][0]).toBe(
      'http://127.0.0.1:1234/api/runs/run-1/messages/event-0/files/12/content',
    )
    await expect(backend.attachmentContent('../other', 'event-0', 0)).rejects.toThrow('引用无效')
    await expect(backend.attachmentContent('run-1', 'event-0', 64)).rejects.toThrow('引用无效')
  })
})

describe('local document extraction', () => {
  it('sends raw bytes and an encoded basename to the authenticated local endpoint', async () => {
    const backend = new Backend('unused', 'unused')
    vi.spyOn(backend, 'start').mockResolvedValue('http://127.0.0.1:1234')
    const fetch = vi.fn(async () => new Response(JSON.stringify({ text: '[page 1]\nPDF text' })))
    vi.stubGlobal('fetch', fetch)
    const bytes = Buffer.from([37, 80, 68, 70, 0, 255])
    await expect(backend.extractDocument('中文 & notes.pdf', bytes)).resolves.toBe(
      '[page 1]\nPDF text',
    )
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
    expect(new URL(url).pathname).toBe('/api/documents/extract')
    expect(new URL(url).searchParams.get('name')).toBe('中文 & notes.pdf')
    expect(init.body).toEqual(new Uint8Array(bytes))
    expect(init.headers).toMatchObject({
      'Content-Type': 'application/octet-stream',
      Authorization: expect.stringMatching(/^Bearer [a-f0-9]+$/),
    })
    expect(init.redirect).toBe('error')
  })

  it('surfaces extraction errors and rejects a malformed successful response', async () => {
    const backend = new Backend('unused', 'unused')
    vi.spyOn(backend, 'start').mockResolvedValue('http://127.0.0.1:1234')
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValue(
          new Response(JSON.stringify({ error: '扫描件需要 OCR' }), { status: 400 }),
        ),
    )
    await expect(backend.extractDocument('scan.pdf', Buffer.from('fixture'))).rejects.toThrow(
      '扫描件需要 OCR',
    )
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}')))
    await expect(backend.extractDocument('scan.pdf', Buffer.from('fixture'))).rejects.toThrow(
      '解析结果无效',
    )
  })
})

describe('local runtime initialization', () => {
  it('starts history immediately while credential decryption remains pending', async () => {
    vi.useFakeTimers()
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    let unlock!: () => void
    const load = new Promise<[]>((resolve) => {
      unlock = () => resolve([])
    })
    const models = new ModelRestoration(backend, () => load)
    const restoration = models.restore()
    const started = backend.start()
    expect(spawn).toHaveBeenCalledOnce()
    child.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await started
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(JSON.stringify([{ id: 'saved-run' }]))),
    )
    expect(await backend.request({ path: '/api/runs', method: 'GET' })).toEqual([
      { id: 'saved-run' },
    ])
    await vi.advanceTimersByTimeAsync(60_000)
    expect(models.snapshot().status).toBe('loading')
    expect(child.kill).not.toHaveBeenCalled()
    unlock()
    await restoration
    expect(models.snapshot()).toEqual({ status: 'ready' })
  })

  it('keeps model failure retryable and deduplicates concurrent unlock requests', async () => {
    const backend = { request: vi.fn() }
    const load = vi
      .fn()
      .mockRejectedValueOnce(new Error('fixture keychain cancelled'))
      .mockResolvedValue([])
    const models = new ModelRestoration(backend, load)
    const first = models.restore()
    expect(models.restore()).toBe(first)
    await first
    expect(models.snapshot()).toMatchObject({
      status: 'error',
      message: expect.stringContaining('本地历史仍可查看'),
    })
    await models.restore()
    expect(models.snapshot()).toEqual({ status: 'ready' })
    expect(load).toHaveBeenCalledTimes(2)
    expect(backend.request).not.toHaveBeenCalled()
  })

  it('re-registers models after failure without creating or resuming a task', async () => {
    const input = {
      protocol: 'responses' as const,
      base_url: 'http://127.0.0.1:1234',
      api_key: 'fixture-only',
      model: 'fixture-model',
      context_window: 128_000,
      max_output_tokens: 8_192,
      reasoning_mode: 'off' as const,
      reasoning_effort: 'medium',
    }
    const backend = {
      request: vi.fn().mockRejectedValueOnce(new Error('runtime exited')).mockResolvedValue({}),
    }
    const models = new ModelRestoration(backend, async () => [input])
    await models.restore()
    expect(models.snapshot().status).toBe('error')
    await models.restore()
    expect(models.snapshot().status).toBe('ready')
    for (const [request] of backend.request.mock.calls)
      expect(request).toEqual({ path: '/api/models', method: 'POST', body: input })
  })

  it('keeps the startup timeout as the cause when terminating the process produces exit zero', async () => {
    vi.useFakeTimers()
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    const started = backend.start()
    const rejected = expect(started).rejects.toThrow('did not become ready within 30 seconds')
    await vi.advanceTimersByTimeAsync(30_000)
    await rejected
    expect(child.kill).toHaveBeenCalledOnce()
    child.emit('exit', 0, null)
    await expect(backend.request({ path: '/api/config', method: 'GET' })).rejects.toThrow(
      'did not become ready within 30 seconds',
    )
    const onError = vi.fn()
    await backend.watch('fixture-run', new AbortController().signal, vi.fn(), onError)
    expect(onError).toHaveBeenCalledWith(expect.stringContaining('请重新启动 Open Harness'))
    expect(onError).toHaveBeenCalledWith(expect.stringContaining('did not become ready'))
    expect(spawn).toHaveBeenCalledOnce()
  })

  it('keeps unexpected runtime exits failed until explicitly restarted', async () => {
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    const started = backend.start()
    child.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await started
    child.emit('exit', 0, null)
    await expect(backend.request({ path: '/api/config', method: 'GET' })).rejects.toThrow(
      'stopped (0). 请重新启动 Open Harness',
    )
  })
})

describe('local runtime shutdown', () => {
  it('restarts only after failure, rotates credentials and never replays a failed mutation', async () => {
    const old = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    const started = backend.start()
    old.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await started
    await expect(backend.restartAfterFailure()).rejects.toThrow('仍在运行')
    Object.assign(old, { exitCode: 1 })
    old.emit('exit', 1, null)
    const fetchMock = vi.fn().mockResolvedValue(new Response('{}'))
    vi.stubGlobal('fetch', fetchMock)
    await expect(backend.request({ path: '/api/runs', method: 'POST', body: {} })).rejects.toThrow(
      'stopped',
    )
    const next = runtimeProcess()
    const restarted = backend.restartAfterFailure()
    expect(backend.restartAfterFailure()).toBe(restarted)
    await Promise.resolve()
    next.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:5678' }) + '\n')
    await restarted
    expect(fetchMock).not.toHaveBeenCalled()
    expect(vi.mocked(spawn).mock.calls[0][2]?.env?.OPENHARNESS_API_TOKEN).not.toEqual(
      vi.mocked(spawn).mock.calls[1][2]?.env?.OPENHARNESS_API_TOKEN,
    )
    old.emit('error', new Error('late event from dead process'))
    await expect(backend.request({ path: '/api/config', method: 'GET' })).resolves.toEqual({})
    expect(fetchMock.mock.calls[0][0]).toBe('http://127.0.0.1:5678/api/config')
  })

  it('closes the managed input pipe and lets the runtime finish cleanup', async () => {
    vi.useFakeTimers()
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    const started = backend.start()
    child.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await started
    expect(vi.mocked(spawn).mock.calls[0][2]?.env).toMatchObject({
      OPENHARNESS_EXIT_ON_STDIN_EOF: '1',
    })
    const stopped = backend.stop()
    expect(child.stdin.writableEnded).toBe(true)
    expect(child.kill).not.toHaveBeenCalled()
    child.emit('exit', 0, null)
    await stopped
    expect(vi.getTimerCount()).toBe(0)
  })

  it('uses a bounded hard stop only when graceful cleanup does not finish', async () => {
    vi.useFakeTimers()
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    const started = backend.start()
    child.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await started
    const stopped = backend.stop()
    await vi.advanceTimersByTimeAsync(9_999)
    expect(child.kill).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1)
    expect(child.kill).toHaveBeenCalledWith('SIGKILL')
    await vi.advanceTimersByTimeAsync(2_000)
    await stopped
    expect(vi.getTimerCount()).toBe(0)
  })
})
