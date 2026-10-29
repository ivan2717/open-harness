import type { ApprovalMode, ExecutionPolicy, UpdateRunPermissionsInput } from '../shared/contracts'
import type { ControlPermission, PermissionTarget } from '../shared/extensions'

function fields(value: unknown, allowed: string[]): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Invalid permissions')
  const input = value as Record<string, unknown>
  if (Object.keys(input).some((key) => !allowed.includes(key)))
    throw new Error('Unknown permission field')
  return input
}
function text(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !value.trim() ||
    value.length > 4096 ||
    /[\u0000-\u001f\u007f]/.test(value)
  )
    throw new Error('Invalid permission target')
  return value
}
function approvalMode(value: unknown): ApprovalMode {
  if (value !== 'ask' && value !== 'auto_review' && value !== 'full_access')
    throw new Error('Invalid approval mode')
  return value
}
export function executionPolicy(value: unknown): ExecutionPolicy | undefined {
  if (value === undefined) return undefined
  const input = fields(value, ['approval_mode', 'max_steps', 'max_agents'])
  const result: ExecutionPolicy = {}
  if (input.approval_mode !== undefined) result.approval_mode = approvalMode(input.approval_mode)
  for (const name of ['max_steps', 'max_agents'] as const) {
    const limit = input[name]
    if (limit === undefined) continue
    if (
      limit !== null &&
      (typeof limit !== 'number' ||
        !Number.isInteger(limit) ||
        limit < 1 ||
        limit > (name === 'max_agents' ? 8 : 4_294_967_295))
    )
      throw new Error('Invalid execution limit')
    result[name] = limit as number | null
  }
  return result
}
export function runPermissionsInput(value: unknown): UpdateRunPermissionsInput {
  const input = fields(value, ['approval_mode', 'permission_mode'])
  if (
    input.permission_mode !== undefined &&
    input.permission_mode !== 'read_only' &&
    input.permission_mode !== 'workspace_write'
  )
    throw new Error('Invalid permission mode')
  return {
    approval_mode: approvalMode(input.approval_mode),
    ...(input.permission_mode === undefined ? {} : { permission_mode: input.permission_mode }),
  }
}
function permissionTarget(value: unknown): PermissionTarget {
  const input = fields(value, ['kind', 'bundle_id', 'origin', 'name', 'uri'])
  const kind = input.kind
  const field =
    kind === 'application'
      ? 'bundle_id'
      : kind === 'origin'
        ? 'origin'
        : kind === 'mcp_resource'
          ? 'uri'
          : 'name'
  if (
    !['application', 'origin', 'mcp_resource', 'mcp_tool', 'mcp_prompt'].includes(String(kind)) ||
    Object.keys(input).some((key) => key !== 'kind' && key !== field)
  )
    throw new Error('Invalid permission target')
  const target = text(input[field])
  if (kind === 'origin') {
    const url = new URL(target)
    if (
      !['https:', 'http:'].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.origin !== target
    )
      throw new Error('Invalid permission origin')
  }
  return { kind, [field]: target } as PermissionTarget
}
export function controlPermission(value: unknown): ControlPermission {
  const input = fields(value, ['server_id', 'server_name', 'capability', 'target', 'lifetime'])
  const capability = input.capability
  if (
    capability !== 'chrome' &&
    capability !== 'browser' &&
    capability !== 'computer' &&
    capability !== 'mcp'
  )
    throw new Error('Invalid permission capability')
  if (input.lifetime !== 'session' && input.lifetime !== 'always')
    throw new Error('Invalid permission lifetime')
  const target = input.target == null ? null : permissionTarget(input.target)
  if (
    target &&
    (capability === 'computer'
      ? target.kind !== 'application'
      : capability === 'mcp'
        ? !target.kind.startsWith('mcp_')
        : target.kind !== 'origin')
  )
    throw new Error('Permission target does not match capability')
  return {
    server_id: text(input.server_id),
    server_name: text(input.server_name),
    capability,
    target,
    lifetime: input.lifetime,
  }
}
