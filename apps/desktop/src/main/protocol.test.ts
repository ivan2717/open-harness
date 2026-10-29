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
    expect(apiRequest({ operation: 'stopCommand', id: 'run-1', commandId: 'cmd-2' })).toEqual({
      path: '/api/runs/run-1/workspace/commands/cmd-2/stop',
      method: 'POST',
    })
    for (const commandId of ['../cmd-2', '/cancel', '', null])
      expect(() => apiRequest({ operation: 'stopCommand', id: 'run-1', commandId })).toThrow()
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
  it('validates command approval scope and exposes only the fixed revocation route', () => {
    for (const scope of ['once', 'session', 'always']) {
      expect(
        apiRequest({
          operation: 'approveCommand',
          id: 'task',
          approvalId: 'command',
          allow: true,
          scope,
        }),
      ).toEqual({
        path: '/api/runs/task/approvals/command',
        method: 'POST',
        body: { allow: true, scope },
      })
    }
    for (const scope of ['forever', '', true, null, {}, ['session']]) {
      expect(() =>
        apiRequest({
          operation: 'approveCommand',
          id: 'task',
          approvalId: 'command',
          allow: true,
          scope,
        }),
      ).toThrow('Invalid approval scope')
    }
    expect(apiRequest({ operation: 'revokeCommandGrants', id: 'task' })).toEqual({
      path: '/api/runs/task/workspace/command-grants/revoke',
      method: 'POST',
    })
    expect(() => apiRequest({ operation: 'revokeCommandGrants', id: '../task' })).toThrow()
  })
  it('changes conversation model settings through a fixed validated route', () => {
    expect(
      apiRequest({
        operation: 'updateRunModel',
        id: 'run-1',
        input: { model_id: 'model-2', reasoning_effort: 'high', api_key: 'ignored' },
      }),
    ).toEqual({
      path: '/api/runs/run-1/model',
      method: 'PATCH',
      body: { model_id: 'model-2', reasoning_effort: 'high' },
    })
    for (const reasoning_effort of [undefined, null]) {
      expect(
        apiRequest({
          operation: 'updateRunModel',
          id: 'run-1',
          input: { model_id: 'model-2', reasoning_effort },
        }).body,
      ).toEqual({ model_id: 'model-2', reasoning_effort: null })
    }
    for (const reasoning_effort of ['', 12, {}, 'HIGH', 'high\n', 'x'.repeat(33)]) {
      expect(() =>
        apiRequest({
          operation: 'updateRunModel',
          id: 'run-1',
          input: { model_id: 'model-2', reasoning_effort },
        }),
      ).toThrow()
      expect(() =>
        apiRequest({
          operation: 'createRun',
          input: { model_id: 'model-2', prompt: 'hello', reasoning_effort },
        }),
      ).toThrow()
    }
    expect(() =>
      apiRequest({ operation: 'updateRunModel', id: '../run', input: { model_id: 'model-2' } }),
    ).toThrow()
    expect(() =>
      apiRequest({ operation: 'updateRunModel', id: 'run-1', input: { model_id: '../models' } }),
    ).toThrow()
    expect(
      apiRequest({
        operation: 'createRun',
        input: { model_id: 'model-2', prompt: 'hello', reasoning_effort: 'low' },
      }).body,
    ).toEqual({ model_id: 'model-2', prompt: 'hello', reasoning_effort: 'low' })
  })
  it('allows attachment-only turns and keeps file snapshots on each input route', () => {
    const files = [{ name: 'README.md', path: '/tmp/README.md', text: 'Referenced content' }]
    expect(
      apiRequest({ operation: 'createRun', input: { prompt: '', model_id: 'm', files } }).body,
    ).toEqual({ prompt: '', model_id: 'm', files })
    for (const operation of ['submitInput', 'resumeRun']) {
      expect(apiRequest({ operation, id: 'run-1', message: '', files }).body).toEqual({
        message: '',
        files,
      })
    }
    expect(apiRequest({ operation: 'resumeRun', id: 'run-1', files }).body).toEqual({
      message: '',
      files,
    })
    expect(
      apiRequest({ operation: 'createRun', input: { prompt: '😀'.repeat(4000), model_id: 'm' } })
        .body,
    ).toHaveProperty('prompt', '😀'.repeat(4000))
    expect(() =>
      apiRequest({ operation: 'createRun', input: { prompt: '😀'.repeat(4001), model_id: 'm' } }),
    ).toThrow('4,000')
  })
  it('preserves original-file metadata on all input routes and refuses invalid or mixed references', () => {
    const files = [
      {
        name: 'large.pdf',
        path: '/tmp/large.pdf',
        attachment: { id: 'a'.repeat(64), size: 90 * 1024 * 1024 },
      },
    ]
    expect(
      apiRequest({ operation: 'createRun', input: { prompt: '', model_id: 'm', files } }).body,
    ).toEqual({ prompt: '', model_id: 'm', files })
    for (const operation of ['submitInput', 'resumeRun'])
      expect(apiRequest({ operation, id: 'run-1', message: '', files }).body).toEqual({
        message: '',
        files,
      })
    for (const file of [
      { ...files[0], text: 'injected' },
      { ...files[0], image: {} },
      { ...files[0], attachment: { id: '../outside', size: 1 } },
      { ...files[0], attachment: { id: 'a'.repeat(64), size: -1 } },
      { ...files[0], attachment: { id: 'a'.repeat(64), size: 101 * 1024 * 1024 } },
    ])
      expect(() =>
        apiRequest({ operation: 'createRun', input: { prompt: '', model_id: 'm', files: [file] } }),
      ).toThrow()
  })

  it('rejects malformed and oversized attachment snapshots at the IPC boundary', () => {
    for (const files of [
      {},
      [{ name: '../file', text: 'a' }],
      [{ name: 'file', path: '/tmp/a\0b', text: 'a' }],
      [{ name: 'file', text: 'a\0b' }],
      [{ name: 'file', text: 'a\fb' }],
      [{ name: 'file', text: 'a\u0085b' }],
      [{ name: 'bad\nname', text: 'a' }],
      [{ name: 'file', path: '/tmp/bad\u0085path', text: 'a' }],
      [{ name: 'file', text: 42 }],
      [{ name: 'file', text: '中'.repeat(90_000) }],
      Array.from({ length: 65 }, () => ({ name: 'file', text: '' })),
      Array.from({ length: 3 }, () => ({ name: 'file', text: 'a'.repeat(256 * 1024) })),
    ])
      expect(() =>
        apiRequest({ operation: 'createRun', input: { prompt: '', model_id: 'm', files } }),
      ).toThrow()
    expect(() =>
      apiRequest({ operation: 'submitInput', id: 'run-1', message: '', files: [] }),
    ).toThrow()
  })

  it('preserves image snapshots on all user input routes and rejects mixed or invalid payloads', () => {
    const image = {
      mime_type: 'image/png',
      data_base64:
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq1sAAAAASUVORK5CYII=',
    }
    const files = [{ name: '截图.png', image }]
    expect(
      apiRequest({ operation: 'createRun', input: { prompt: '', model_id: 'm', files } }).body,
    ).toEqual({ prompt: '', model_id: 'm', files })
    for (const operation of ['submitInput', 'resumeRun'])
      expect(apiRequest({ operation, id: 'run-1', message: '', files }).body).toEqual({
        message: '',
        files,
      })
    for (const invalid of [
      [{ ...files[0], text: 'cannot hide pixels in a text reference' }],
      [{ ...files[0], image: { ...image, mime_type: 'image/jpeg' } }],
      [{ ...files[0], image: { ...image, data_base64: image.data_base64 + '\n' } }],
      Array(5).fill(files[0]),
    ])
      expect(() =>
        apiRequest({ operation: 'submitInput', id: 'run-1', message: '', files: invalid }),
      ).toThrow()
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
