import { describe, expect, it } from 'vitest'
import type { ModelConfiguration } from '../../shared/contracts'
import { validateReasoningCapability, type ReasoningCapability } from '../../shared/model-reasoning'
import { availableReasoningOptions, editableReasoningCapability } from './reasoning-settings'

const initial: ModelConfiguration = {
  id: 'fixture',
  has_api_key: true,
  protocol: 'anthropic_messages',
  base_url: 'https://fixture.example/v1',
  model: 'fixture',
  context_window: 128000,
  max_output_tokens: 8192,
  reasoning_mode: 'standard',
  reasoning_effort: 'high',
}

describe('editable model reasoning settings', () => {
  it.each([
    [8192, 8191],
    [2048, 2047],
  ])(
    'preserves legacy high reasoning with %i output tokens as %i thinking tokens',
    (output, expected) => {
      const capability = editableReasoningCapability({ ...initial, max_output_tokens: output })
      expect(capability.source).toBe('legacy')
      expect(capability.adapter).toBe('anthropic_budget')
      expect(capability.options.find((option) => option.id === 'high')).toEqual({
        id: 'high',
        budget_tokens: expected,
      })
      expect(availableReasoningOptions(capability, output).map((option) => option.id)).toContain(
        initial.reasoning_effort,
      )
      expect(validateReasoningCapability(capability, initial.protocol)).toEqual(capability)
    },
  )

  it.each(['metadata', 'manual', 'legacy'] as const)(
    'keeps an explicit %s capability exactly as saved, without clamping its budgets or labels',
    (source) => {
      const capability: ReasoningCapability = {
        source,
        adapter: 'anthropic_budget',
        options: [
          { id: 'off', enabled: false, label: '不思考' },
          { id: 'brief', budget_tokens: 1024, label: '简短' },
          { id: 'deep', budget_tokens: 16384, label: '深入研究' },
        ],
      }
      expect(
        editableReasoningCapability({
          ...initial,
          reasoning_mode: 'off',
          reasoning_capability: capability,
        }),
      ).toBe(capability)
      expect(capability.options[2].budget_tokens).toBe(16384)
    },
  )

  it('does not reinterpret legacy effort controls on other protocols', () => {
    const capability = editableReasoningCapability({ ...initial, protocol: 'responses' })
    expect(capability).toEqual({
      source: 'legacy',
      adapter: 'effort',
      options: [
        { id: 'low', effort: 'low' },
        { id: 'medium', effort: 'medium' },
        { id: 'high', effort: 'high' },
      ],
    })
  })
})
