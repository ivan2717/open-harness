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
          session_scope: { kind: 'browser' },
        },
      ],
    })
    expect(html).toContain('协作任务：调查产品资料')
    expect(html).toContain('允许本次')
    expect(html).toContain('允许本会话及协作任务')
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
    expect(html).not.toContain('允许本会话及协作任务')
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
    expect(html).toContain('调查产品资料：Chrome')
    expect(html).toContain('本会话及协作任务控制')
    expect(html).toContain('停止控制')
  })
})
