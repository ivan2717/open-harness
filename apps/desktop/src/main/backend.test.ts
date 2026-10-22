import { afterEach, describe, expect, it, vi } from 'vitest'
import { Backend } from './backend'
import { apiRequest } from './protocol'

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

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
