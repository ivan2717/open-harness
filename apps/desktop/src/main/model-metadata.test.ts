import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ConfigureModelInput } from '../shared/contracts'
import { fetchModelCapacity, fetchModelMetadata } from './model-metadata'

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
  it('discovers explicitly declared relay controls even with manual capacities', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        data: [
          {
            id: input.model,
            reasoning_capability: {
              source: 'catalog',
              adapter: 'deepseek',
              options: [
                { id: 'off', enabled: false },
                { id: 'strong', effort: 'max' },
              ],
            },
          },
        ],
      }),
    )
    expect(
      await fetchModelMetadata(
        { ...input, context_window: 32000, max_output_tokens: 4096 },
        request,
      ),
    ).toEqual({
      capacity: undefined,
      reasoning: {
        source: 'metadata',
        adapter: 'deepseek',
        options: [
          { id: 'off', enabled: false },
          { id: 'strong', effort: 'max' },
        ],
      },
    })
    expect(request).toHaveBeenCalledTimes(1)
    expect(request.mock.calls[0][1]?.method).toBe('GET')
  })

  it('does not infer native wire mapping from an effort list on an unknown relay', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        data: [
          {
            id: input.model,
            effort: { supported_levels: ['low', 'high'] },
            supported_parameters: ['reasoning'],
          },
        ],
      }),
    )
    expect(await fetchModelMetadata(input, request)).toBeUndefined()
  })

  it('maps official DeepSeek effort metadata separately for each protocol', async () => {
    for (const protocol of ['chat_completions', 'responses', 'anthropic_messages'] as const) {
      const request = vi.fn<typeof fetch>().mockResolvedValue(
        Response.json({
          data: [
            {
              id: input.model,
              effort: { supported_levels: ['low', 'high', 'max'], default_level: 'high' },
            },
          ],
        }),
      )
      const metadata = await fetchModelMetadata(
        {
          ...input,
          protocol,
          base_url:
            protocol === 'anthropic_messages'
              ? 'https://api.deepseek.com/anthropic'
              : 'https://api.deepseek.com/v1',
        },
        request,
      )
      expect(metadata?.reasoning).toEqual({
        source: 'metadata',
        adapter:
          protocol === 'responses'
            ? 'effort'
            : protocol === 'chat_completions'
              ? 'deepseek'
              : 'messages_effort',
        options: [
          protocol === 'responses' ? { id: 'none', effort: 'none' } : { id: 'off', enabled: false },
          ...['low', 'high', 'max'].map((id) => ({ id, effort: id })),
        ],
      })
      expect(request).toHaveBeenCalledTimes(1)
    }
  })

  it('ignores malformed reasoning without losing valid capacity metadata', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        data: [
          {
            id: input.model,
            context_window: 32768,
            reasoning_capability: {
              adapter: 'deepseek',
              options: [{ id: 'off', enabled: false, effort: 'high' }],
            },
          },
        ],
      }),
    )
    expect(await fetchModelMetadata(input, request)).toEqual({
      capacity: { context_window: 32768, max_output_tokens: undefined },
      reasoning: undefined,
    })
  })
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

describe('official provider metadata dialects', () => {
  const discover = async (
    base_url: string,
    protocol: ConfigureModelInput['protocol'],
    body: object,
    model = input.model,
  ) => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(Response.json(body))
    const result = await fetchModelMetadata({ ...input, base_url, protocol, model }, request)
    expect(request).toHaveBeenCalledTimes(1)
    return { result, request }
  }

  it('reads Claude effort flags and adaptive support without inventing a disable option', async () => {
    const { result, request } = await discover('https://api.anthropic.com', 'anthropic_messages', {
      id: 'resolved-model',
      max_input_tokens: 1000000,
      max_tokens: 64000,
      capabilities: {
        effort: {
          supported: true,
          low: { supported: true },
          medium: { supported: false },
          high: { supported: true },
          xhigh: { supported: true },
          max: { supported: false },
        },
        thinking: {
          supported: true,
          types: { adaptive: { supported: true }, disabled: { supported: false } },
        },
      },
    })
    expect(result?.reasoning).toEqual({
      source: 'metadata',
      adapter: 'anthropic_adaptive',
      options: ['low', 'high', 'xhigh'].map((id) => ({ id, effort: id })),
    })
    expect(request.mock.calls[0][0]).toBe('https://api.anthropic.com/v1/models/fixture-model')
    expect(request.mock.calls[0][1]?.headers).toEqual({
      Accept: 'application/json',
      'x-api-key': 'fixture-key',
      'anthropic-version': '2026-10-21',
    })
  })

  it('keeps the legacy metadata header for Messages-compatible relays', async () => {
    const { request } = await discover('https://messages-relay.example/v1', 'anthropic_messages', {
      id: input.model,
    })
    expect(request.mock.calls[0][1]?.headers).toEqual({
      Accept: 'application/json',
      'x-api-key': 'fixture-key',
      'anthropic-version': '2023-06-01',
    })
  })

  it('rejects contradictory and malformed official capability declarations', async () => {
    for (const capabilities of [
      { effort: { supported: false, high: { supported: true } }, thinking: { supported: false } },
      {
        effort: { supported: false },
        thinking: { supported: false, types: { enabled: { supported: true } } },
      },
      { effort: { supported: true, low: { supported: 'true' } } },
      { effort: { supported: false, high: { supported: 'true' } }, thinking: { supported: false } },
      {
        effort: { supported: false },
        thinking: { supported: false, types: { adaptive: 'supported' } },
      },
    ]) {
      const { result } = await discover('https://api.anthropic.com/v1', 'anthropic_messages', {
        id: input.model,
        capabilities,
        max_input_tokens: 200000,
      })
      expect(result?.reasoning).toBeUndefined()
      expect(result?.capacity?.context_window).toBe(200000)
    }
    const { result } = await discover(
      'https://openrouter.ai/api/v1',
      'responses',
      {
        data: {
          id: 'vendor/model',
          reasoning: { supported_efforts: ['none', 'high'], mandatory: 'true' },
        },
      },
      'vendor/model',
    )
    expect(result?.reasoning).toBeUndefined()
  })

  it('does not force Claude thinking on when only effort is declared', async () => {
    const { result } = await discover('https://api.anthropic.com/v1', 'anthropic_messages', {
      id: input.model,
      capabilities: {
        effort: { supported: true, high: { supported: true } },
        thinking: { supported: false },
      },
    })
    expect(result?.reasoning).toEqual({
      source: 'metadata',
      adapter: 'messages_effort_only',
      options: [{ id: 'high', effort: 'high' }],
    })
  })

  it('uses editable budget presets only when Claude declares manual thinking support', async () => {
    const { result } = await discover('https://api.anthropic.com/v1', 'anthropic_messages', {
      id: input.model,
      capabilities: {
        effort: { supported: false },
        thinking: {
          supported: true,
          types: { enabled: { supported: true }, disabled: { supported: true } },
        },
      },
    })
    expect(result?.reasoning).toMatchObject({ source: 'preset', adapter: 'anthropic_budget' })
    expect(result?.reasoning?.options.map((option) => option.id)).toEqual([
      'off',
      'budget_1024',
      'budget_4096',
      'budget_8192',
    ])
  })

  it('distinguishes explicit absence of controls from incomplete Claude metadata', async () => {
    for (const capabilities of [
      { effort: { supported: false }, thinking: { supported: false } },
      { effort: { supported: true } },
    ]) {
      const { result } = await discover('https://api.anthropic.com/v1', 'anthropic_messages', {
        id: input.model,
        capabilities,
      })
      expect(result?.reasoning).toEqual(
        capabilities.effort.supported
          ? undefined
          : { source: 'metadata', adapter: 'none', options: [] },
      )
    }
  })

  it('reads xAI model capability values rather than model prefixes or defaults', async () => {
    const { result, request } = await discover('https://api.x.ai/v1', 'responses', {
      data: [
        {
          id: input.model,
          context_length: 256000,
          capabilities: {
            reasoning_effort: ['low', 'high', 'xhigh'],
            default_reasoning_effort: 'high',
          },
        },
      ],
    })
    expect(result?.reasoning).toEqual({
      source: 'metadata',
      adapter: 'effort',
      options: ['low', 'high', 'xhigh'].map((id) => ({ id, effort: id })),
    })
    expect(result?.capacity?.context_window).toBe(256000)
    expect(request.mock.calls[0][0]).toBe('https://api.x.ai/v1/models')
  })

  it('keeps explicit empty effort lists empty rather than supplying preset choices', async () => {
    const xai = await discover('https://api.x.ai/v1', 'responses', {
      data: [{ id: input.model, capabilities: { reasoning_effort: [] } }],
    })
    expect(xai.result?.reasoning).toEqual({ source: 'metadata', adapter: 'none', options: [] })
    for (const reasoning of [
      { supported_efforts: [] },
      { supported_efforts: ['none'], mandatory: true },
    ]) {
      const router = await discover(
        'https://openrouter.ai/api/v1',
        'responses',
        { data: { id: 'vendor/model', reasoning } },
        'vendor/model',
      )
      expect(router.result?.reasoning).toEqual({ source: 'metadata', adapter: 'none', options: [] })
    }
    for (const reasoning_effort of [undefined, null, ['high', 'high'], ['high', 1]]) {
      const malformed = await discover('https://api.x.ai/v1', 'responses', {
        data: [{ id: input.model, capabilities: { reasoning_effort } }],
      })
      expect(malformed.result?.reasoning).toBeUndefined()
    }
  })

  it('uses OpenRouter single-model discovery and honors mandatory reasoning', async () => {
    const { result, request } = await discover(
      'https://openrouter.ai/api/v1',
      'chat_completions',
      {
        data: {
          id: 'vendor/model',
          context_length: 200000,
          reasoning: { supported_efforts: ['none', 'high', 'low'], mandatory: true },
          top_provider: { max_completion_tokens: 32000 },
        },
      },
      'vendor/model',
    )
    expect(request.mock.calls[0][0]).toBe('https://openrouter.ai/api/v1/model/vendor/model')
    expect(result?.reasoning?.options.map((option) => option.id)).toEqual(['high', 'low'])
    expect(result?.capacity).toEqual({ context_window: 200000, max_output_tokens: 32000 })
  })

  it('respects OpenRouter null effort semantics but does not guess from missing parameters', async () => {
    for (const reasoning of [
      { supported_efforts: null, mandatory: true },
      { supports_max_tokens: true },
    ]) {
      const { result } = await discover(
        'https://openrouter.ai/api/v1',
        'responses',
        { data: { id: 'vendor/model', reasoning } },
        'vendor/model',
      )
      expect(result?.reasoning?.options.map((option) => option.id)).toEqual(
        'supported_efforts' in reasoning
          ? ['minimal', 'low', 'medium', 'high', 'xhigh', 'max']
          : undefined,
      )
    }
  })

  it('reads Gemini native metadata on the same origin without inventing effort values', async () => {
    const { result, request } = await discover(
      'https://generativelanguage.googleapis.com/v1beta/openai',
      'chat_completions',
      {
        name: 'models/fixture-model',
        thinking: true,
        inputTokenLimit: 1048576,
        outputTokenLimit: 65536,
      },
    )
    expect(request.mock.calls[0][0]).toBe(
      'https://generativelanguage.googleapis.com/v1beta/models/fixture-model',
    )
    expect(request.mock.calls[0][1]?.headers).toEqual({
      Accept: 'application/json',
      'x-goog-api-key': 'fixture-key',
    })
    expect(result).toEqual({
      capacity: { context_window: 1048576, max_output_tokens: 65536 },
      reasoning: undefined,
    })
  })

  it('maps explicitly unsupported Gemini and Kimi reasoning to no controls', async () => {
    for (const [base_url, body] of [
      [
        'https://generativelanguage.googleapis.com/v1beta/openai',
        { name: 'models/fixture-model', thinking: false },
      ],
      ['https://api.moonshot.cn/v1', { data: [{ id: input.model, supports_reasoning: false }] }],
    ] as const) {
      const { result } = await discover(base_url, 'chat_completions', body)
      expect(result?.reasoning).toEqual({ source: 'metadata', adapter: 'none', options: [] })
    }
  })

  it('reads Qwen native model info while leaving undeclared efforts to presets', async () => {
    const { result, request } = await discover(
      'https://dashscope.aliyuncs.com/compatible-mode/v1',
      'chat_completions',
      {
        output: {
          models: [
            {
              model: input.model,
              capabilities: ['Reasoning'],
              model_info: { context_window: 1000000, max_output_tokens: 32768 },
            },
          ],
        },
      },
    )
    expect(request.mock.calls[0][0]).toBe(
      'https://dashscope.aliyuncs.com/api/v1/models?model=fixture-model',
    )
    expect(result).toEqual({
      capacity: { context_window: 1000000, max_output_tokens: 32768 },
      reasoning: undefined,
    })
  })

  it('does not recognize provider lookalikes or accept another model from native lists', async () => {
    const { result } = await discover('https://api.x.ai.evil.example/v1', 'chat_completions', {
      data: [{ id: input.model, capabilities: { reasoning_effort: ['high'] } }],
    })
    expect(result).toBeUndefined()
    const wrong = await discover(
      'https://dashscope.aliyuncs.com/compatible-mode/v1',
      'chat_completions',
      { output: { models: [{ model: 'other', model_info: { context_window: 1000000 } }] } },
    )
    expect(wrong.result).toBeUndefined()
  })
})
