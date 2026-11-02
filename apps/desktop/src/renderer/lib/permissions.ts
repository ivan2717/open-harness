import { queryOptions, type QueryClient } from '@tanstack/react-query'
import type { ApprovalMode, CommandGrant } from '../../shared/contracts'
import type { ControlPermission, PermissionTarget } from '../../shared/extensions'

export const approvalModes: { id: ApprovalMode; label: string; description: string }[] = [
  { id: 'ask', label: '请求批准', description: '访问互联网或使用外部工具时询问你。' },
  {
    id: 'auto_review',
    label: '帮我批准',
    description: '自动批准低风险操作，不确定时询问你。',
  },
  {
    id: 'full_access',
    label: '完全访问权限',
    description: '无需逐次批准，可联网并读写电脑上的文件。',
  },
]
export const permissionOptions = queryOptions({
  queryKey: ['permissions'],
  queryFn: () => window.harness.permissions(),
  staleTime: 0,
})
export function runPermissionOptions(id: string) {
  return queryOptions({
    queryKey: ['run-permissions', id],
    queryFn: () => window.harness.runPermissions(id),
    staleTime: 0,
  })
}
export function refreshPermissions(client: QueryClient) {
  for (const key of [
    'permissions',
    'run-permissions',
    'workspace',
    'run-extensions',
    'extension-settings',
    'run',
    'runs',
  ])
    void client.invalidateQueries({ queryKey: [key] })
}
export function targetLabel(target?: PermissionTarget | null): string {
  if (!target) return '整个能力范围'
  switch (target.kind) {
    case 'origin':
      return target.origin
    case 'application':
      return target.bundle_id
    case 'mcp_tool':
      return `工具 · ${target.name}`
    case 'mcp_resource':
      return `资源 · ${target.uri}`
    case 'mcp_prompt':
      return `提示模板 · ${target.name}`
  }
}
export function permissionLabel(permission: ControlPermission): string {
  return `${permission.server_name} · ${targetLabel(permission.target)}`
}
export function permissionKey(permission: ControlPermission): string {
  return `${permission.server_id}:${permission.lifetime ?? 'session'}:${JSON.stringify(permission.target ?? null)}`
}
export function commandGrantLabel(grant: CommandGrant): string {
  return `${grant.access === 'read_only' ? '项目只读' : '可修改项目'} · ${grant.network_access === 'allowed' ? '允许联网' : '网络关闭'}`
}
