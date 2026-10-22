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
  it('defaults project access to read-only and rejects malformed permission grants', () => {
    expect(
      apiRequest({
        operation: 'createRun',
        input: { prompt: 'Inspect', model_id: 'm', workspace_path: '/tmp/project' },
      }).body,
    ).toEqual({
      prompt: 'Inspect',
      model_id: 'm',
      workspace_path: '/tmp/project',
      permission_mode: 'read_only',
    })
    for (const workspace_path of ['relative/path', '/tmp/evil\0path', 23]) {
      expect(() =>
        apiRequest({
          operation: 'createRun',
          input: { prompt: 'Inspect', model_id: 'm', workspace_path },
        }),
      ).toThrow()
    }
    expect(() =>
      apiRequest({
        operation: 'createRun',
        input: {
          prompt: 'Inspect',
          model_id: 'm',
          workspace_path: '/tmp/p',
          permission_mode: 'unrestricted',
        },
      }),
    ).toThrow()
    expect(() =>
      apiRequest({
        operation: 'createRun',
        input: { prompt: 'Inspect', model_id: 'm', permission_mode: 'workspace_write' },
      }),
    ).toThrow()
  })
  it('confines approval and revert requests to fixed routes and relative files', () => {
    expect(
      apiRequest({ operation: 'approveCommand', id: 'run-1', approvalId: 'cmd-2', allow: false }),
    ).toEqual({
      path: '/api/runs/run-1/approvals/cmd-2',
      method: 'POST',
      body: { allow: false },
    })
    for (const allow of ['true', 1, undefined]) {
      expect(() =>
        apiRequest({ operation: 'approveCommand', id: 'run-1', approvalId: 'cmd-2', allow }),
      ).toThrow()
    }
    expect(() =>
      apiRequest({
        operation: 'approveCommand',
        id: 'run-1',
        approvalId: '../cancel',
        allow: true,
      }),
    ).toThrow()
    for (const path of ['../secret', '/tmp/file', 'C:\\file', '..\\file', 'file\0txt']) {
      expect(() => apiRequest({ operation: 'revertFile', id: 'run-1', path })).toThrow()
    }
    expect(apiRequest({ operation: 'revertFile', id: 'run-1', path: 'src/main.rs' }).body).toEqual({
      path: 'src/main.rs',
    })
  })
  it('resumes the existing run with an optional bounded message', () => {
    expect(apiRequest({ operation: 'resumeRun', id: 'run-1' })).toEqual({
      path: '/api/runs/run-1/resume',
      method: 'POST',
      body: {},
    })
    expect(
      apiRequest({ operation: 'resumeRun', id: 'run-1', message: 'Check current files first' })
        .body,
    ).toEqual({ message: 'Check current files first' })
    expect(() => apiRequest({ operation: 'resumeRun', id: 'run-1', message: '' })).toThrow()
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
