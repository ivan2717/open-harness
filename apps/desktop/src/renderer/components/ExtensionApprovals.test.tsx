import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { RunExtensions } from '../../shared/extensions'
import { ExtensionApprovals } from './ExtensionApprovals'

function render(extensions: RunExtensions): string {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  client.setQueryData(['run-extensions', 'root'], extensions)
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <ExtensionApprovals runId="root" running />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}

describe('delegated external tool approvals', () => {
  it('keeps permission revocation available after all active targets have been released', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [],
      controls: [],
      control_permissions: [{ server_id: 'chrome', server_name: 'Chrome', capability: 'chrome' }],
    })
    expect(html).toContain('Chrome')
    expect(html).toContain('本会话及协作任务控制')
    expect(html).toContain('停止控制')
  })
  it('identifies the requesting task and explains the family scope of reusable approval', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [
        {
          id: 'mcp-1',
          server_id: 'browser',
          server_name: '独立浏览器',
          operation: 'browser_open',
          arguments: { url: 'https://example.com' },
          source_run_id: 'child',
          task_title: '调查产品资料',
          session_scope: { kind: 'browser_origin', origin: 'https://example.com' },
          permission_target: { kind: 'origin', origin: 'https://example.com' },
          can_allow_capability: true,
        },
      ],
    })
    expect(html).toContain('协作任务：调查产品资料')
    expect(html).toContain('允许本会话及协作任务访问上述目标')
    expect(html).toContain('https://example.com')
    expect(html).toContain('扩大到 独立浏览器 的所有目标')
    expect(html).toMatch(/type="checkbox"[^>]*>/)
    expect(html).not.toContain('checked=""')
    expect(html).toContain('始终允许')
    expect(html).toContain('取消')
    expect(html).toMatch(/>允许<\/button>/)
    expect(html).not.toContain('允许本次')
    expect(html).not.toContain('拒绝')
  })

  it('does not offer reusable approval for an ordinary external operation', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [
        {
          id: 'mcp-2',
          server_id: 'mail',
          server_name: '邮件',
          operation: 'send',
          arguments: {},
          source_run_id: 'child',
          task_title: '撰写邮件',
        },
      ],
    })
    expect(html).toContain('协作任务：撰写邮件')
    expect(html).not.toContain('始终允许')
    expect(html).toContain('取消')
    expect(html).toMatch(/>允许<\/button>/)
  })

  it('keeps the family revocation action visible for child controls', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [],
      controls: [
        {
          server_id: 'chrome',
          server_name: 'Chrome',
          remembered: true,
          scope: { kind: 'chrome_tab', tab_id: 42, origin: 'https://example.com' },
          source_run_id: 'child',
          task_title: '调查产品资料',
        },
      ],
    })
    expect(html).toContain('Chrome')
    expect(html).not.toContain('标签页 42')
    expect(html).toContain('本会话及协作任务控制')
    expect(html).toContain('停止控制')
  })
  it('does not repeat tab identifiers or origins for controls shared across collaborating tasks', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [],
      controls: [42, 43].map((tab_id) => ({
        server_id: 'chrome',
        server_name: 'Chrome',
        remembered: true,
        scope: { kind: 'chrome_tab', tab_id, origin: 'https://example.com' },
        source_run_id: `child-${tab_id}`,
        task_title: `Task ${tab_id}`,
      })),
    })
    expect(html.match(/Chrome/g)).toHaveLength(1)
    expect(html).not.toContain('example.com')
    expect(html).toContain('停止控制')
  })
  it('explains capability approval without claiming that each new tab needs authorization', () => {
    const html = render({
      skills: [],
      diagnostics: [],
      approvals: [
        {
          id: 'mcp-3',
          server_id: 'chrome',
          server_name: 'Chrome · chrome',
          operation: 'chrome_open',
          arguments: {},
          session_scope: { kind: 'chrome_new_tab', origin: 'https://example.com' },
        },
      ],
    })
    expect(html).toContain('允许使用Chrome？')
    expect(html).toContain('专用标签页分组')
    expect(html).toContain('你已有的标签页仍需单独共享')
    expect(html).not.toContain('会话授权仅适用于这个新标签页和网站')
  })
})
