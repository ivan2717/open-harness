import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { WindowsScriptSettings, windowsScriptEnvironmentKey } from './WindowsScriptSettings'

afterEach(() => vi.unstubAllGlobals())

function render(platform: string, ready: boolean) {
  const prepare = vi.fn()
  vi.stubGlobal('window', { harness: { platform, prepareWindowsScriptEnvironment: prepare } })
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } })
  client.setQueryData(windowsScriptEnvironmentKey, {
    ready,
    paths: ['C:\\'],
    missing_paths: ready ? [] : ['C:\\'],
  })
  try {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <WindowsScriptSettings />
      </QueryClientProvider>,
    )
    expect(prepare).not.toHaveBeenCalled()
    return html
  } finally {
    client.clear()
  }
}

describe('Windows script environment settings', () => {
  it.each(['darwin', 'linux'])('does not show Windows setup on %s', (platform) => {
    expect(render(platform, false)).toBe('')
  })

  it('offers an explicit preparation action and explains the limited elevation', () => {
    const html = render('win32', false)
    expect(html).toContain('需要准备')
    expect(html).toContain('准备脚本环境')
    expect(html).toContain('Windows 管理员确认')
    expect(html).toContain('不会将任务或脚本提升为管理员')
    expect(html).toContain('系统重启后可能需要再次准备')
    expect(html).not.toContain('disabled')
  })

  it('shows readiness and a read-only recheck after preparation', () => {
    const html = render('win32', true)
    expect(html).toContain('已就绪')
    expect(html).toContain('检查状态')
    expect(html).not.toContain('准备脚本环境')
  })
})
