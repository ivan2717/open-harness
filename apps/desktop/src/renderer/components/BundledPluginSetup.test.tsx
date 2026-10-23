import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { BundledPlugin } from '../../shared/extensions'
import { BundledPluginSetup } from './BundledPluginSetup'

function render(permissions: BundledPlugin['permissions']): string {
  const client = new QueryClient()
  client.setQueryData(
    ['bundled-plugins'],
    [{ id: 'computer-use', name: '电脑操作', available: true, permissions }],
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <BundledPluginSetup
        plugin={{
          id: 'computer-use',
          name: '电脑操作',
          description: '',
          available: true,
          permissions,
        }}
      />
    </QueryClientProvider>,
  )
}

describe('computer permission presentation', () => {
  it('reports each confirmed permission independently', () => {
    const html = render({ accessibility: true, screenRecording: false, helperPath: '/helper' })
    expect(html).toContain('辅助功能 · 已授权')
    expect(html).toContain('屏幕录制 · 未授权')
  })

  it('does not tell the user to reauthorize when the check failed', () => {
    const html = render({
      accessibility: null,
      screenRecording: null,
      helperPath: '/helper',
      error: '权限检查超时，请重试。',
    })
    expect(html).toContain('辅助功能 · 检查失败')
    expect(html).toContain('屏幕录制 · 检查失败')
    expect(html).toContain('role="alert"')
    expect(html).toContain('权限检查超时，请重试。')
    expect(html).not.toContain(' · 未授权')
    expect(html).not.toContain('待授权')
  })

  it('treats a missing result as unknown instead of denied', () => {
    const html = render(undefined)
    expect(html).toContain('辅助功能 · 检查失败')
    expect(html).not.toContain(' · 未授权')
  })
})
