import { describe, expect, it } from 'vitest'
import type { ConfigureModelInput } from './contracts'
import { resolveModelLimits } from './model-limits'

const input: ConfigureModelInput = {
  protocol: 'responses',
  base_url: 'https://api.openai.com/v1',
  api_key: 'test-only',
  model: 'gpt-6-luna',
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}

describe('automatic model limits', () => {
  it('uses exact published models only on their official protocol and endpoint', () => {
    expect(resolveModelLimits(input)).toEqual({
      context_window: 1_050_000,
      max_output_tokens: 8192,
      context_source: 'catalog',
      output_source: 'catalog',
    })
    for (const change of [
      { base_url: 'https://gateway.example/v1' },
      { base_url: 'https://api.openai.com.evil.example/v1' },
      { base_url: 'https://api.openai.com:8443/v1' },
      { base_url: 'https://api.openai.com/custom/v1' },
      { base_url: 'https://api.openai.com/v1?token=secret' },
      { model: 'gpt-6-luna-custom' },
      { protocol: 'anthropic_messages' as const },
    ])
      expect(resolveModelLimits({ ...input, ...change })).toMatchObject({
        context_window: 8192,
        max_output_tokens: 2048,
        context_source: 'fallback',
      })
    expect(
      resolveModelLimits({
        ...input,
        base_url: 'https://api.deepseek.com/v1/',
        model: 'deepseek-flash',
        protocol: 'chat_completions',
      }).context_window,
    ).toBe(1_048_576)
    expect(
      resolveModelLimits({
        ...input,
        base_url: 'https://api.anthropic.com/v1',
        model: 'claude-sonnet-5-5',
        protocol: 'anthropic_messages',
      }).context_window,
    ).toBe(1_000_000)
  })

  it('prefers validated metadata and keeps output below both its limit and the input reserve', () => {
    expect(resolveModelLimits(input, { context_window: 32768, max_output_tokens: 4096 })).toEqual({
      context_window: 32768,
      max_output_tokens: 4096,
      context_source: 'metadata',
      output_source: 'metadata',
    })
    expect(
      resolveModelLimits(input, { context_window: 1024, max_output_tokens: 512 }),
    ).toMatchObject({
      context_window: 1024,
      max_output_tokens: 256,
    })
    expect(
      resolveModelLimits({ ...input, model: 'unknown' }, { context_window: 131072 }),
    ).toMatchObject({ context_window: 131072, max_output_tokens: 8192 })
    expect(
      resolveModelLimits(input, { context_window: Number.NaN, max_output_tokens: 8_000_001 }),
    ).toEqual(resolveModelLimits(input))
  })

  it('honors explicit overrides and rejects invalid or input-starving values', () => {
    expect(
      resolveModelLimits(
        { ...input, context_window: 16384, max_output_tokens: 4096 },
        { context_window: 32768, max_output_tokens: 8192 },
      ),
    ).toEqual({
      context_window: 16384,
      max_output_tokens: 4096,
      context_source: 'override',
      output_source: 'override',
    })
    for (const change of [
      { context_window: 0 },
      { context_window: 1024.5 },
      { context_window: Number.NaN },
      { max_output_tokens: 63 },
      { max_output_tokens: Number.POSITIVE_INFINITY },
      { context_window: 1024, max_output_tokens: 960 },
    ])
      expect(() => resolveModelLimits({ ...input, ...change })).toThrow()
  })
})
