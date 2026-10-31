import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { BundledPlugin } from '../../shared/extensions'
import { BundledPluginSetup } from './BundledPluginSetup'

function render(
  permissions: BundledPlugin['permissions'],
  desktop?: BundledPlugin['desktop'],
): string {
  const client = new QueryClient()
  client.setQueryData(
    ['bundled-plugins'],
    [{ id: 'computer-use', name: '电脑操作', available: true, permissions, desktop }],
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
          desktop,
        }}
      />
    </QueryClientProvider>,
  )
}

describe('computer permission presentation', () => {
  it('reports each confirmed permission independently', () => {
    const html = render({ accessibility: true, screenRecording: false, helperPath: '/helper' })
    expect(html).toContain('辅助功能 · 已授权')
    expect(html).toContain('屏幕录制 · 未生效')
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
    expect(html).not.toContain(' · 未生效')
    expect(html).not.toContain('待授权')
  })

  it('treats a missing result as unknown instead of denied', () => {
    const html = render(undefined)
    expect(html).toContain('辅助功能 · 检查失败')
    expect(html).not.toContain(' · 未授权')
  })

  it.each([true, false, null])(
    'shows Windows desktop state %s without macOS permission actions',
    (available) => {
      const html = render(undefined, {
        available,
        helperPath: 'C:\\Open Harness\\computer-use\\windows\\bin\\OpenHarnessComputerUse.exe',
        ...(available === null ? { error: '桌面状态检查超时，请重试。' } : {}),
      })
      expect(html).toContain(
        available === true ? '桌面可用' : available === false ? '桌面暂不可用' : '桌面状态检查失败',
      )
      expect(html).toContain('检查状态')
      expect(html).toContain('点击和输入需要将该应用置于前台')
      expect(html).not.toContain('检查权限')
      expect(html).not.toContain('辅助功能 ·')
      expect(html).not.toContain('屏幕录制 ·')
      if (available === null) expect(html).toContain('桌面状态检查超时，请重试。')
    },
  )
})
