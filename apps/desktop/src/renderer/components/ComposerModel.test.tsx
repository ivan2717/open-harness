import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import type { ModelInfo } from '../../shared/contracts'
import { ComposerModel } from './ComposerModel'

vi.mock('@tanstack/react-router', () => ({
  Link: ({ to, children, ...props }: { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
}))
vi.mock('./ComposerMenu', () => ({
  ComposerMenu: ({
    label,
    title,
    disabled,
    trigger,
    children,
  }: {
    label: string
    title?: string
    disabled?: boolean
    trigger: ReactNode
    children: ReactNode
  }) => (
    <div>
      <button aria-label={label} title={title} disabled={disabled}>
        {trigger}
      </button>
      <div role="menu">{children}</div>
    </div>
  ),
}))

const model: ModelInfo = {
  id: 'reasoning-model',
  name: 'Thinking model',
  description: '',
  available: true,
  context_window: 1_000_000,
  reasoning: { default_effort: 'high', supported_efforts: ['low', 'medium', 'high'] },
}

function render(overrides: Partial<Parameters<typeof ComposerModel>[0]> = {}) {
  return renderToStaticMarkup(
    <ComposerModel
      models={[model]}
      modelId={model.id}
      onChange={vi.fn()}
      onReasoningChange={vi.fn()}
      {...overrides}
    />,
  )
}

describe('composer model and reasoning menu', () => {
  it('distinguishes an inherited default from an explicit conversation override', () => {
    const inherited = render()
    expect(inherited).toContain('composer-reasoning-label">高')
    expect(inherited).toContain('aria-checked="true" class="composer-menu-item"><span>默认（高）')
    const explicit = render({ reasoningEffort: 'low' })
    expect(explicit).toContain('composer-reasoning-label">低')
    expect(explicit).toContain('aria-checked="true" class="composer-menu-item"><span>低')
    expect(explicit).toContain('aria-checked="false" class="composer-menu-item"><span>默认（高）')
  })

  it('only exposes efforts advertised by the configured model', () => {
    const html = render()
    expect(html).toContain('role="group" aria-label="思考强度"')
    expect(html).not.toContain('Ultra')
    expect(html).not.toContain('极高')
    expect(render({ models: [{ ...model, reasoning: undefined }] })).not.toContain(
      'aria-label="思考强度"',
    )
  })

  it('uses declared labels and keeps explicit disabling distinct from the service default', () => {
    const configured: ModelInfo = {
      ...model,
      reasoning: {
        default_effort: null,
        supported_efforts: ['off', 'thorough'],
        capability: {
          source: 'manual',
          adapter: 'deepseek',
          options: [
            { id: 'off', enabled: false },
            { id: 'thorough', label: '仔细思考', effort: 'max' },
          ],
        },
      },
    }
    const html = render({ models: [configured], reasoningEffort: 'off' })
    expect(html).toContain('aria-checked="false" class="composer-menu-item"><span>服务默认')
    expect(html).toContain('aria-checked="true" class="composer-menu-item"><span>关闭思考')
    expect(html).toContain('仔细思考')
    expect(html).not.toContain('<span>中</span>')
  })

  it('uses budget and toggle controls instead of claiming universal effort levels', () => {
    const budget: ModelInfo = {
      ...model,
      reasoning: {
        default_effort: 'budget_4096',
        supported_efforts: ['budget_4096'],
        capability: {
          source: 'metadata',
          adapter: 'anthropic_budget',
          options: [{ id: 'budget_4096', budget_tokens: 4096 }],
        },
      },
    }
    const html = render({ models: [budget] })
    expect(html).toContain('aria-label="思考预算"')
    expect(html).toContain('4,096 tokens')
    const toggle: ModelInfo = {
      ...model,
      reasoning: {
        default_effort: null,
        supported_efforts: ['on', 'off'],
        capability: {
          source: 'manual',
          adapter: 'chat_toggle',
          options: [
            { id: 'on', enabled: true },
            { id: 'off', enabled: false },
          ],
        },
      },
    }
    const toggleHtml = render({ models: [toggle] })
    expect(toggleHtml).toContain('aria-label="思考开关"')
    expect(toggleHtml).toContain('开启思考')
    expect(toggleHtml).toContain('关闭思考')
    expect(toggleHtml).not.toContain('<span>高</span>')
  })

  it('does not invent controls for a legacy unknown configuration', () => {
    const html = render({
      models: [
        {
          ...model,
          reasoning: {
            default_effort: null,
            supported_efforts: [],
            capability: { source: 'unknown', adapter: 'none', options: [] },
          },
        },
      ],
    })
    expect(html).not.toContain('role="group" aria-label="思考强度"')
    expect(html).not.toContain('<span>低</span>')
  })

  it('labels thinking.type options as a switch instead of effort levels', () => {
    const html = render({
      models: [
        {
          ...model,
          reasoning: {
            default_effort: null,
            supported_efforts: ['on', 'off'],
            capability: {
              source: 'preset',
              adapter: 'thinking_toggle',
              options: [
                { id: 'on', enabled: true },
                { id: 'off', enabled: false },
              ],
            },
          },
        },
      ],
    })
    expect(html).toContain('aria-label="思考开关"')
    expect(html).toContain('开启思考')
    expect(html).toContain('关闭思考')
    expect(html).not.toContain('aria-label="思考强度"')
  })

  it('marks generic preset options without implying that the service declared them', () => {
    const configured: ModelInfo = {
      ...model,
      reasoning: {
        default_effort: null,
        supported_efforts: ['high'],
        capability: {
          source: 'preset',
          adapter: 'effort',
          options: [{ id: 'high', effort: 'high' }],
        },
      },
    }
    const html = render({ models: [configured] })
    expect(html).toContain('默认预设')
    expect(html).toContain('尚未获得当前模型的明确声明')
    expect(html).toContain('aria-checked="true" class="composer-menu-item"><span>服务默认')
    const declared: ModelInfo = {
      ...configured,
      reasoning: {
        ...configured.reasoning!,
        capability: { ...configured.reasoning!.capability!, source: 'metadata' },
      },
    }
    expect(render({ models: [declared] })).not.toContain('默认预设')
  })

  it('lets an old undeclared override return to default without advertising it as available', () => {
    const html = render({
      reasoningEffort: 'high',
      models: [
        {
          ...model,
          reasoning: {
            default_effort: null,
            supported_efforts: [],
            capability: { source: 'unknown', adapter: 'none', options: [] },
          },
        },
      ],
    })
    expect(html).toContain('role="group" aria-label="思考强度"')
    expect(html).toContain('aria-checked="false" class="composer-menu-item"><span>服务默认')
    expect(html).toContain(
      'aria-checked="true" disabled="" class="composer-menu-item"><span>当前保留（高）',
    )
    expect(html).not.toContain('<span>高</span>')
    expect(html).not.toContain('<span>中</span>')
  })

  it('keeps the picker usable when the current model is unavailable', () => {
    const html = render({
      models: [
        { ...model, available: false },
        { ...model, id: 'replacement', name: 'Replacement' },
      ],
    })
    expect(html).toContain('aria-label="选择模型" title="切换模型和思考强度"')
    expect(html).toContain('aria-checked="true" disabled=""')
    expect(html).toContain('Replacement')
  })

  it('explains why the selector is temporarily disabled without claiming the model is fixed', () => {
    const html = render({
      disabled: true,
      disabledReason: '任务运行中，停止或等待本轮结束后可切换。',
    })
    expect(html).toContain('title="任务运行中，停止或等待本轮结束后可切换。" disabled=""')
    expect(html).not.toContain('模型已固定')
  })
})
