import { Children, isValidElement, type ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import type { ReasoningCapability } from '../../shared/model-reasoning'
import { ModelReasoningCompatibility, ModelReasoningDefault } from './ModelReasoningSettings'

function renderDefault(capability: ReasoningCapability, value = '', outputLimit = 8192) {
  return renderToStaticMarkup(
    <ModelReasoningDefault
      capability={capability}
      outputLimit={outputLimit}
      value={value}
      onChange={vi.fn()}
    />,
  )
}

describe('model reasoning settings', () => {
  it('keeps a legacy unknown configuration usable with the service default', () => {
    const html = renderDefault({ source: 'unknown', adapter: 'none', options: [] })
    expect(html).toContain('使用服务默认设置')
    expect(html.match(/<option /g)).toHaveLength(1)
    expect(html).not.toContain('value="low"')
    expect(html).not.toContain('DeepSeek')
  })

  it('identifies fallback presets without presenting them as a model declaration', () => {
    const capability: ReasoningCapability = {
      source: 'preset',
      adapter: 'effort',
      options: [{ id: 'high', effort: 'high' }],
    }
    const html = renderDefault(capability)
    expect(html).toContain('<option value="" selected="">使用服务默认设置</option>')
    expect(html).toContain('使用默认预设，可在高级设置中调整；具体选项以服务支持为准。')
    expect(html).not.toContain('已读取当前服务提供的思考选项')
    expect(renderDefault({ ...capability, source: 'metadata' })).toContain(
      '已读取当前服务提供的思考选项',
    )
  })

  it('copies the currently resolved options into an independent manual override', () => {
    const resolved: ReasoningCapability = {
      source: 'metadata',
      adapter: 'deepseek',
      options: [{ id: 'careful', effort: 'max', label: '仔细思考' }],
    }
    const onChange = vi.fn()
    const tree = ModelReasoningCompatibility({ protocol: 'chat_completions', resolved, onChange })
    let customize: (() => void) | undefined
    function visit(node: ReactNode) {
      Children.forEach(node, (child) => {
        if (!isValidElement<{ children?: ReactNode; onClick?: () => void }>(child)) return
        if (child.type === 'button' && child.props.children === '自定义当前选项')
          customize = child.props.onClick
        visit(child.props.children)
      })
    }
    visit(tree)
    expect(customize).toBeDefined()
    customize!()
    const override = onChange.mock.calls[0][0] as ReasoningCapability
    expect(override).toEqual({ ...resolved, source: 'manual' })
    override.options[0].effort = 'high'
    expect(resolved.options[0].effort).toBe('max')
    expect(resolved.source).toBe('metadata')
    const manualHtml = renderToStaticMarkup(
      <ModelReasoningCompatibility
        protocol="chat_completions"
        value={override}
        resolved={resolved}
        onChange={onChange}
      />,
    )
    expect(manualHtml).not.toContain('自定义当前选项')
  })

  it('renders only the declared choices and preserves the default-versus-disabled distinction', () => {
    const capability: ReasoningCapability = {
      source: 'metadata',
      adapter: 'deepseek',
      options: [
        { id: 'off', enabled: false },
        { id: 'low', effort: 'low' },
        { id: 'max', effort: 'max' },
      ],
    }
    const html = renderDefault(capability, 'off')
    expect(html).toContain('<option value="">使用服务默认设置</option>')
    expect(html).toContain('<option value="off" selected="">关闭思考</option>')
    expect(html).toContain('<option value="max">最高</option>')
    expect(html).not.toContain('value="medium"')
  })

  it('filters output-incompatible budgets while leaving the declaration intact', () => {
    const capability: ReasoningCapability = {
      source: 'manual',
      adapter: 'anthropic_budget',
      options: [
        { id: 'off', enabled: false },
        { id: 'budget_1024', budget_tokens: 1024 },
        { id: 'budget_8192', budget_tokens: 8192 },
      ],
    }
    const html = renderDefault(capability)
    expect(html).toContain('1,024 tokens')
    expect(html).not.toContain('value="budget_8192"')
    expect(capability.options).toHaveLength(3)
    expect(renderDefault(capability, '', 16384)).toContain('8,192 tokens')
  })

  it('keeps protocol-specific templates in advanced compatibility settings', () => {
    const render = (protocol: 'responses' | 'anthropic_messages') =>
      renderToStaticMarkup(<ModelReasoningCompatibility protocol={protocol} onChange={vi.fn()} />)
    expect(render('responses')).not.toContain('value="deepseek"')
    expect(render('responses')).not.toContain('自适应思考')
    expect(render('anthropic_messages')).toContain('自适应思考 + 强度')
    expect(render('anthropic_messages')).toContain('思考 token 预算')
    expect(render('anthropic_messages')).toContain('手动模板请按服务说明调整')
  })

  it.each(['chat_toggle', 'thinking_toggle'] as const)(
    'allows %s to retain only the supported direction',
    (adapter) => {
      const html = renderToStaticMarkup(
        <ModelReasoningCompatibility
          protocol="chat_completions"
          value={{ source: 'manual', adapter, options: [{ id: 'on', enabled: true }] }}
          onChange={vi.fn()}
        />,
      )
      expect(html).toContain('aria-label="移除开启思考选项"')
      expect(html).not.toContain('aria-label="移除关闭思考选项"')
      expect(html).toContain('恢复此兼容模板')
    },
  )

  it('does not add a thinking toggle to an effort-only Messages configuration', () => {
    const html = renderToStaticMarkup(
      <ModelReasoningCompatibility
        protocol="anthropic_messages"
        value={{
          source: 'manual',
          adapter: 'messages_effort_only',
          options: [{ id: 'high', effort: 'high' }],
        }}
        onChange={vi.fn()}
      />,
    )
    expect(html).toContain('思考选项 1 发送值')
    expect(html).not.toContain('添加关闭选项')
    expect(html).not.toContain('移除开启思考选项')
  })
})
