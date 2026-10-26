import { describe, expect, it } from 'vitest'
import {
  resolveReasoningCapability,
  officialReasoningProvider,
  reasoningPresets,
  validateReasoningCapability,
  type ReasoningCapability,
} from './model-reasoning'
import type { ConfigureModelInput } from './contracts'

const input: ConfigureModelInput = {
  protocol: 'responses',
  base_url: 'https://api.openai.com/v1',
  model: 'gpt-6-luna',
  api_key: 'fixture-key',
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const manual: ReasoningCapability = {
  source: 'manual',
  adapter: 'effort',
  options: [{ id: 'thorough', label: '深入', effort: 'xhigh' }],
}

describe('connection-specific reasoning capabilities', () => {
  it('uses provider presets for new models without a model-name catalog', () => {
    const expected = ['low', 'medium', 'high', 'xhigh', 'max']
    expect(resolveReasoningCapability(input).source).toBe('preset')
    expect(resolveReasoningCapability(input).options.map((option) => option.id)).toEqual(expected)
    expect(
      resolveReasoningCapability({ ...input, model: 'future-unlisted-model' }).options.map(
        (option) => option.id,
      ),
    ).toEqual(expected)
    expect(
      resolveReasoningCapability({ ...input, base_url: 'https://relay.example/v1' }).options.map(
        (option) => option.id,
      ),
    ).toEqual(['low', 'medium', 'high'])
    for (const base_url of [
      'https://api.openai.com.evil.example/v1',
      'https://api.openai.com@evil.example/v1',
      'http://api.openai.com/v1',
      'https://api.openai.com:8443/v1',
      'https://api.openai.com/v1?target=relay',
    ])
      expect(officialReasoningProvider({ base_url })).toBeUndefined()
  })

  it('selects vendor wire formats separately from the protocol', () => {
    const cases = [
      ['https://api.deepseek.com/v1', 'chat_completions', 'deepseek'],
      ['https://api.deepseek.com/anthropic', 'anthropic_messages', 'messages_effort'],
      ['https://api.anthropic.com/v1', 'anthropic_messages', 'messages_effort_only'],
      ['https://dashscope.aliyuncs.com/compatible-mode/v1', 'chat_completions', 'chat_toggle'],
      ['https://relay.example/v1', 'anthropic_messages', 'anthropic_budget'],
    ] as const
    for (const [base_url, protocol, adapter] of cases)
      expect(resolveReasoningCapability({ ...input, base_url, protocol })).toMatchObject({
        source: 'preset',
        adapter,
      })
  })

  it('resolves explicit overrides before current metadata before provider presets', () => {
    const metadata: ReasoningCapability = { ...manual, source: 'metadata' }
    expect(resolveReasoningCapability(input, metadata)).toEqual(metadata)
    expect(
      resolveReasoningCapability(input, { source: 'metadata', adapter: 'none', options: [] }),
    ).toEqual({ source: 'metadata', adapter: 'none', options: [] })
    const budgetPreset: ReasoningCapability = {
      source: 'preset',
      adapter: 'anthropic_budget',
      options: [{ id: 'budget_1024', budget_tokens: 1024 }],
    }
    expect(
      resolveReasoningCapability({ ...input, protocol: 'anthropic_messages' }, budgetPreset),
    ).toEqual(budgetPreset)
    expect(
      resolveReasoningCapability({ ...input, reasoning_capability: manual }, metadata),
    ).toEqual(manual)
    expect(
      resolveReasoningCapability(
        { ...input, reasoning_capability: { source: 'manual', adapter: 'none', options: [] } },
        metadata,
      ).options,
    ).toEqual([])
  })

  it('retains legacy explicit declarations without inferring support from old defaults', () => {
    const relay = { ...input, base_url: 'https://relay.example/v1' }
    expect(resolveReasoningCapability(relay).source).toBe('preset')
    expect(resolveReasoningCapability({ ...relay, reasoning_mode: 'standard' })).toMatchObject({
      source: 'legacy',
      adapter: 'effort',
    })
  })

  it('validates protocol templates and automatic presets', () => {
    for (const protocol of ['chat_completions', 'responses', 'anthropic_messages'] as const)
      for (const { capability } of reasoningPresets(protocol))
        expect(validateReasoningCapability(capability, protocol)).toEqual(capability)
    for (const protocol of ['chat_completions', 'responses', 'anthropic_messages'] as const) {
      const capability = resolveReasoningCapability({ ...input, protocol })
      expect(validateReasoningCapability(capability, protocol)).toEqual(capability)
    }
  })

  it('rejects invalid fields, conflicting options and incompatible adapters', () => {
    const malformed = [
      { ...manual, source: 'verified' },
      { ...manual, source: 'unknown' },
      { ...manual, headers: { Authorization: 'secret' } },
      { ...manual, adapter: 'deepseek' },
      { ...manual, options: [manual.options[0], manual.options[0]] },
      { ...manual, options: [{ id: 'high', effort: 'high', enabled: true }] },
      { ...manual, options: [{ id: 'high', effort: 'high', arbitrary: true }] },
      { ...manual, options: [{ id: '', effort: 'high' }] },
      { ...manual, options: [{ id: 'high', effort: 'a'.repeat(33) }] },
      { ...manual, options: [{ id: 'high', label: 'bad\nlabel', effort: 'high' }] },
    ]
    for (const capability of malformed)
      expect(() => validateReasoningCapability(capability, 'responses')).toThrow()
    expect(() =>
      validateReasoningCapability(
        {
          source: 'manual',
          adapter: 'anthropic_budget',
          options: [{ id: 'tiny', budget_tokens: 512 }],
        },
        'anthropic_messages',
      ),
    ).toThrow()
    expect(() =>
      validateReasoningCapability(
        {
          source: 'manual',
          adapter: 'deepseek',
          options: [{ id: 'off', enabled: false, effort: 'high' }],
        },
        'chat_completions',
      ),
    ).toThrow()
  })
})
