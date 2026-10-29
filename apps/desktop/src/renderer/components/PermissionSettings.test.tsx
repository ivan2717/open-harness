import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, it, vi } from 'vitest'
import type { ControlPermission } from '../../shared/extensions'
import { PermissionList, PermissionSettings } from './PermissionSettings'
import { permissionKey, permissionLabel } from '../lib/permissions'

const permissions: ControlPermission[] = [
  {
    server_id: 'chrome',
    server_name: 'Chrome',
    capability: 'chrome',
    target: { kind: 'origin', origin: 'https://example.com' },
    lifetime: 'always',
  },
  {
    server_id: 'chrome',
    server_name: 'Chrome',
    capability: 'chrome',
    target: { kind: 'origin', origin: 'https://other.example' },
    lifetime: 'always',
  },
  {
    server_id: 'native',
    server_name: '电脑操作',
    capability: 'computer',
    target: { kind: 'application', bundle_id: 'com.example.notes' },
    lifetime: 'session',
  },
  {
    server_id: 'mail',
    server_name: '邮件',
    capability: 'mcp',
    target: { kind: 'mcp_tool', name: 'send_message' },
    lifetime: 'always',
  },
]
describe('permission management', () => {
  it('keeps different targets on the same service distinct and explains lifetime', () => {
    const html = renderToStaticMarkup(
      <PermissionList permissions={permissions} busy={false} onRevoke={vi.fn()} />,
    )
    expect(new Set(permissions.map(permissionKey)).size).toBe(4)
    for (const permission of permissions) expect(html).toContain(permissionLabel(permission))
    expect(html).toContain('跨会话保留 · 始终允许')
    expect(html).toContain('本会话及协作任务')
    expect(html).not.toContain('完全访问')
  })
  it('keeps authorization details behind a secondary settings entry', () => {
    const client = new QueryClient()
    client.setQueryData(
      ['permissions'],
      permissions.filter((item) => item.lifetime === 'always'),
    )
    try {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={client}>
          <PermissionSettings />
        </QueryClientProvider>,
      )
      expect(html).toContain('管理授权与审查记录')
      expect(html).toContain('<details')
      expect(html).not.toContain('始终允许的访问')
      expect(html).not.toContain('https://example.com')
      expect(html).not.toContain('com.example.notes')
    } finally {
      client.clear()
    }
  })
})
