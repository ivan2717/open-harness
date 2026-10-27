import { afterEach, describe, expect, it, vi } from 'vitest'
import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import { resolve } from 'node:path'
import { Backend, backendSandboxEnvironment } from './backend'
import { apiRequest } from './protocol'
import { initializeRuntime, watchInitializedRuntime } from './runtime-startup'

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

describe('local runtime initialization', () => {
  it('does not start a process or readiness timer while keychain initialization is pending', async () => {
    vi.useFakeTimers()
    const child = runtimeProcess()
    const backend = new Backend(fixtureBinary, 'fixture-directory')
    let unlock!: () => void
    const load = new Promise<[]>((resolve) => {
      unlock = () => resolve([])
    })
    const initialization = initializeRuntime(backend, () => load, vi.fn())
    const cancelled = new AbortController()
    const active = new AbortController()
    const cancelledWatch = vi.fn().mockResolvedValue(undefined)
    const activeWatch = vi.fn().mockResolvedValue(undefined)
    const stopped = watchInitializedRuntime(initialization, cancelled.signal, cancelledWatch)
    const watching = watchInitializedRuntime(initialization, active.signal, activeWatch)
    cancelled.abort()
    await vi.advanceTimersByTimeAsync(60_000)
    expect(spawn).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
    expect(activeWatch).not.toHaveBeenCalled()
    unlock()
    await Promise.resolve()
    expect(spawn).toHaveBeenCalledOnce()
    expect(vi.getTimerCount()).toBe(1)
    child.stdout.write(JSON.stringify({ event: 'ready', address: '127.0.0.1:1234' }) + '\n')
    await Promise.all([initialization, stopped, watching])
    expect(cancelledWatch).not.toHaveBeenCalled()
    expect(activeWatch).toHaveBeenCalledOnce()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('starts the runtime even if saved models cannot be decrypted', async () => {
    const backend = { start: vi.fn().mockResolvedValue('http://127.0.0.1:1234'), request: vi.fn() }
    const report = vi.fn()
    await initializeRuntime(
      backend,
      async () => {
        throw new Error('fixture keychain unavailable')
      },
      report,
    )
    expect(backend.start).toHaveBeenCalledOnce()
    expect(backend.request).not.toHaveBeenCalled()
    expect(report).toHaveBeenCalledWith(expect.stringContaining('本地会话记录仍然可用'))
  })

  it('does not subscribe until restored model registration finishes', async () => {
    const input = {
      protocol: 'responses' as const,
      base_url: 'http://127.0.0.1:1234',
      api_key: 'fixture-only',
      model: 'fixture-model',
      context_window: 128_000,
      max_output_tokens: 8_192,
      reasoning_mode: 'off' as const,
      reasoning_effort: 'medium' as const,
    }
    let registered!: () => void
    const pending = new Promise<void>((resolve) => {
      registered = resolve
    })
    const backend = {
      start: vi.fn().mockResolvedValue('http://127.0.0.1:1234'),
      request: vi.fn(() => pending),
    }
    const initialization = initializeRuntime(backend, async () => [input], vi.fn())
    const watch = vi.fn().mockResolvedValue(undefined)
    const watching = watchInitializedRuntime(initialization, new AbortController().signal, watch)
    await Promise.resolve()
    expect(backend.request).toHaveBeenCalledWith({
      path: '/api/models',
      method: 'POST',
      body: input,
    })
    expect(watch).not.toHaveBeenCalled()
    registered()
    await watching
    expect(backend.start).toHaveBeenCalledOnce()
    expect(watch).toHaveBeenCalledOnce()
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

  it('requires an app restart after a runtime that was ready exits unexpectedly', async () => {
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
