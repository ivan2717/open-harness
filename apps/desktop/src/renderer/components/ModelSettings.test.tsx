import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { ModelConfiguration } from '../../shared/contracts'
import { ModelSettings } from './ModelSettings'

const initial: ModelConfiguration = {
  id: 'fixture',
  has_api_key: true,
  protocol: 'responses',
  base_url: 'https://api.openai.com/v1',
  model: 'gpt-6-luna',
  context_window: 1_050_000,
  max_output_tokens: 128000,
  output_limit_mode: 'auto',
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}

function render(profile: ModelConfiguration): string {
  const client = new QueryClient()
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <ModelSettings initial={profile} />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}

describe('editable model output limits', () => {
  it('does not display the automatic planning reserve as a manual output cap', () => {
    const html = render(initial)
    expect(html).toMatch(/placeholder="自动（服务默认）"[^>]*name="max_output_tokens"[^>]*value=""/)
    expect(html).toContain('由模型服务决定单次输出上限')
  })

  it('preserves a legacy 8192 limit and exposes automatic reset', () => {
    const html = render({ ...initial, output_limit_mode: undefined, max_output_tokens: 8192 })
    expect(html).toMatch(/name="max_output_tokens"[^>]*value="8192"/)
    expect(html).toContain('恢复容量自动设置')
    expect(html).toContain('使用你填写的上限')
  })

  it('shows a concrete default for protocols requiring an output cap', () => {
    const html = render({
      ...initial,
      protocol: 'anthropic_messages',
      base_url: 'https://gateway.example/v1',
      model: 'unknown',
      max_output_tokens: 32768,
    })
    expect(html).toMatch(/placeholder="自动（32768）"[^>]*name="max_output_tokens"[^>]*value=""/)
    expect(html).toContain('当前输出预算为 32,768 tokens')
  })

  it('retains a discovered automatic reserve when editing a model with large reasoning budgets', () => {
    const html = render({
      ...initial,
      protocol: 'anthropic_messages',
      base_url: 'https://gateway.example/v1',
      model: 'custom',
      max_output_tokens: 64000,
      reasoning_default: 'deep',
      reasoning_capability: {
        source: 'manual',
        adapter: 'anthropic_budget',
        options: [{ id: 'deep', budget_tokens: 40000 }],
      },
    })
    expect(html).toContain('value="deep" selected=""')
    expect(html).toContain('当前输出预算为 64,000 tokens')
  })
})
