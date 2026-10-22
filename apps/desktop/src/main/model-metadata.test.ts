import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ConfigureModelInput } from '../shared/contracts'
import { fetchModelCapacity } from './model-metadata'

const input: ConfigureModelInput = {
  protocol: 'chat_completions',
  base_url: 'http://127.0.0.1:8100/v1',
  api_key: 'fixture-key',
  model: 'fixture-model',
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
afterEach(() => vi.useRealTimers())

describe('bounded model metadata discovery', () => {
  it('uses one GET with exact list matching and no credential redirects', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        data: [
          { id: 'other-model', context_window: 1_000_000 },
          { id: input.model, context_window: 32768, max_output_tokens: 4096 },
        ],
      }),
    )
    expect(await fetchModelCapacity(input, request)).toEqual({
      context_window: 32768,
      max_output_tokens: 4096,
    })
    expect(request).toHaveBeenCalledTimes(1)
    expect(request).toHaveBeenCalledWith('http://127.0.0.1:8100/v1/models', {
      method: 'GET',
      headers: { Accept: 'application/json', Authorization: 'Bearer fixture-key' },
      redirect: 'error',
      credentials: 'omit',
      signal: expect.any(AbortSignal),
    })
  })

  it('reads Anthropic metadata without adding the output limit to its context window', async () => {
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        Response.json({ id: 'resolved-snapshot', max_input_tokens: 200000, max_tokens: 64000 }),
      )
    expect(await fetchModelCapacity({ ...input, protocol: 'anthropic_messages' }, request)).toEqual(
      { context_window: 200000, max_output_tokens: 64000 },
    )
    expect(request).toHaveBeenCalledWith(
      'http://127.0.0.1:8100/v1/models/fixture-model',
      expect.objectContaining({
        headers: {
          Accept: 'application/json',
          'x-api-key': 'fixture-key',
          'anthropic-version': '2023-06-01',
        },
      }),
    )
  })

  it('uses the documented DeepSeek models endpoint across protocols', async () => {
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        Response.json({ data: [{ id: 'deepseek-flash', context_window: 1048576 }] }),
      )
    expect(
      await fetchModelCapacity(
        {
          ...input,
          base_url: 'https://api.deepseek.com/anthropic/v1',
          model: 'deepseek-flash',
          protocol: 'anthropic_messages',
        },
        request,
      ),
    ).toMatchObject({ context_window: 1048576 })
    expect(request).toHaveBeenCalledWith(
      'https://api.deepseek.com/models',
      expect.objectContaining({
        headers: { Accept: 'application/json', Authorization: 'Bearer fixture-key' },
      }),
    )
  })

  it('skips probing explicit configurations and OpenAI metadata without capacity fields', async () => {
    const request = vi.fn<typeof fetch>()
    expect(
      await fetchModelCapacity(
        { ...input, context_window: 32768, max_output_tokens: 4096 },
        request,
      ),
    ).toBeUndefined()
    expect(
      await fetchModelCapacity({ ...input, base_url: 'https://api.openai.com/v1' }, request),
    ).toBeUndefined()
    expect(request).not.toHaveBeenCalled()
  })

  it('ignores errors, unmatched IDs, malformed fields, and oversized responses', async () => {
    const responses = [
      new Response('', { status: 503 }),
      new Response('', { status: 302, headers: { Location: 'https://elsewhere.example/models' } }),
      new Response('not-json'),
      Response.json({ data: [{ id: 'other-model', context_window: 1_000_000 }] }),
      Response.json({
        data: [{ id: input.model, context_window: '32768', max_output_tokens: null }],
      }),
      Response.json({
        data: [{ id: input.model, context_window: 0, max_output_tokens: 8_000_001 }],
      }),
      new Response('{}', { headers: { 'content-length': String(256 * 1024 + 1) } }),
      new Response(' '.repeat(256 * 1024 + 1)),
    ]
    for (const response of responses) {
      const request = vi.fn<typeof fetch>().mockResolvedValue(response)
      expect(await fetchModelCapacity(input, request)).toBeUndefined()
      expect(request).toHaveBeenCalledTimes(1)
    }
    const request = vi.fn<typeof fetch>().mockRejectedValue(new Error('provider key error'))
    await expect(fetchModelCapacity(input, request)).resolves.toBeUndefined()
  })

  it('aborts the entire discovery after two seconds', async () => {
    vi.useFakeTimers()
    const request = vi.fn<typeof fetch>().mockImplementation(
      (_url, init) =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(new Error('aborted')))
        }),
    )
    const result = fetchModelCapacity(input, request)
    await vi.advanceTimersByTimeAsync(2000)
    await expect(result).resolves.toBeUndefined()
    expect(request).toHaveBeenCalledTimes(1)
  })
})
