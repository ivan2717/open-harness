import { describe, expect, it } from 'vitest'
import { apiRequest, watchRequest } from './protocol'
import { SseDecoder } from './sse'
import { SubscriptionRegistry } from './subscriptions'

describe('desktop bridge request boundary', () => {
  it('maps only fixed operations to local routes', () => {
    expect(apiRequest({ operation: 'getRun', id: 'run_42' })).toEqual({
      path: '/api/runs/run_42',
      method: 'GET',
    })
    expect(apiRequest({ operation: 'submitInput', id: 'abc', message: 'Continue' })).toEqual({
      path: '/api/runs/abc/input',
      method: 'POST',
      body: { message: 'Continue' },
    })
    expect(() => apiRequest({ operation: 'fetch', url: 'https://example.com' })).toThrow()
    expect(() => apiRequest({ operation: 'getRun', id: '../config' })).toThrow()
    expect(() => apiRequest({ operation: 'getRun', id: 'abc?admin=true' })).toThrow()
  })
  it('rejects empty and oversized input without coercion', () => {
    for (const prompt of ['', '  ', 42, 'a'.repeat(64_001)]) {
      expect(() =>
        apiRequest({ operation: 'createRun', input: { prompt, model_id: 'demo' } }),
      ).toThrow()
    }
    expect(() => watchRequest({ id: 'ok', token: {} })).toThrow()
  })
})

describe('SSE framing', () => {
  it('decodes multiline data, CRLF, comments, and split network packets', () => {
    const decoder = new SseDecoder()
    expect(decoder.push(': ping\r\nevent: snap')).toEqual([])
    expect(decoder.push('shot\r\ndata: {"id":\r')).toEqual([])
    expect(decoder.push('\ndata: "abc"}\r\n\r\n')).toEqual([
      { event: 'snapshot', data: '{"id":\n"abc"}' },
    ])
    expect(decoder.push('data: second\n\n')).toEqual([{ event: 'message', data: 'second' }])
  })
  it('rejects unterminated unbounded events', () => {
    expect(() => new SseDecoder().push('a'.repeat(16 * 1024 * 1024 + 1))).toThrow()
  })
})

describe('subscription ownership', () => {
  it('keeps StrictMode replacement subscriptions alive when an old cleanup arrives', () => {
    const registry = new SubscriptionRegistry()
    const old = registry.start(7, 'old-token')
    const replacement = registry.start(7, 'new-token')
    registry.stop(7, 'old-token')
    expect(old.signal.aborted).toBe(true)
    expect(replacement.signal.aborted).toBe(false)
    registry.stopOwner(7)
    expect(replacement.signal.aborted).toBe(true)
  })
  it('isolates owners and cleans all subscriptions on quit', () => {
    const registry = new SubscriptionRegistry()
    const first = registry.start(1, 'same-token')
    const second = registry.start(2, 'same-token')
    registry.stopOwner(1)
    expect(first.signal.aborted).toBe(true)
    expect(second.signal.aborted).toBe(false)
    registry.stopAll()
    expect(second.signal.aborted).toBe(true)
  })
})
