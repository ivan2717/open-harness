import { describe, expect, it } from 'vitest'
import { apiRequest } from './protocol'
import { controlPermission, executionPolicy } from './permission-protocol'

const permission = {
  server_id: 'chrome',
  server_name: 'Chrome',
  capability: 'chrome',
  target: { kind: 'origin', origin: 'https://example.com' },
  lifetime: 'always',
}
describe('permission IPC boundaries', () => {
  it('passes only known execution policy fields to creation', () => {
    const result = apiRequest({
      operation: 'createRun',
      input: {
        prompt: 'test',
        model_id: 'model',
        execution_policy: { approval_mode: 'auto_review', max_steps: null, max_agents: 4 },
      },
    })
    expect(result.body).toMatchObject({
      execution_policy: { approval_mode: 'auto_review', max_steps: null, max_agents: 4 },
    })
    for (const policy of [
      { approval_mode: 'unrestricted' },
      { network: true },
      { max_steps: 0 },
      { max_agents: 9 },
      { max_steps: 1.5 },
      { max_steps: Infinity },
    ])
      expect(() => executionPolicy(policy)).toThrow()
    expect(executionPolicy({ approval_mode: 'ask' })).toEqual({ approval_mode: 'ask' })
    expect(
      apiRequest({
        operation: 'createRun',
        input: {
          prompt: 'test',
          model_id: 'model',
          execution_policy: { approval_mode: 'full_access' },
        },
      }).body,
    ).toMatchObject({ execution_policy: { approval_mode: 'full_access' } })
  })
  it.each(['ask', 'auto_review', 'full_access'])(
    'updates %s without inventing a project file permission',
    (approval_mode) => {
      const result = apiRequest({
        operation: 'updateRunPermissions',
        id: 'root',
        input: { approval_mode },
      })
      expect(result).toEqual({
        path: '/api/runs/root/permissions',
        method: 'PATCH',
        body: { approval_mode },
      })
      expect(result.body).not.toHaveProperty('permission_mode')
    },
  )
  it('validates permission editing and fixed paths', () => {
    expect(
      apiRequest({
        operation: 'updateRunPermissions',
        id: 'root',
        input: { approval_mode: 'auto_review', permission_mode: 'read_only' },
      }),
    ).toEqual({
      path: '/api/runs/root/permissions',
      method: 'PATCH',
      body: { approval_mode: 'auto_review', permission_mode: 'read_only' },
    })
    for (const input of [
      { approval_mode: 'ask', permission_mode: 'full_access' },
      { approval_mode: 'ask', permission_mode: 'read_only', secret: true },
      { approval_mode: 'full_access', permission_mode: null },
      { approval_mode: 'full_access', permission_mode: true },
      { approval_mode: 'full_access', network: true },
      { approval_mode: 'unrestricted' },
      { permission_mode: 'workspace_write' },
    ])
      expect(() => apiRequest({ operation: 'updateRunPermissions', id: 'root', input })).toThrow()
    expect(() => apiRequest({ operation: 'runPermissions', id: '../other' })).toThrow()
    expect(() =>
      apiRequest({ operation: 'revokeRunPermissions', id: 'root', stop: 'yes' }),
    ).toThrow()
    expect(apiRequest({ operation: 'revokeRunPermissions', id: 'root', stop: true })).toEqual({
      path: '/api/runs/root/permissions/revoke',
      method: 'POST',
      body: { stop: true },
    })
  })
  it('preserves the exact permission target and lifetime for revocation', () => {
    expect(apiRequest({ operation: 'revokePermission', permission })).toEqual({
      path: '/api/permissions/revoke',
      method: 'POST',
      body: { permission },
    })
    expect(
      apiRequest({
        operation: 'revokePermission',
        permission: { ...permission, lifetime: 'session' },
        runId: 'child',
      }).body,
    ).toMatchObject({ run_id: 'child', permission: { lifetime: 'session' } })
    for (const target of [
      { kind: 'origin', origin: 'https://user:secret@example.com' },
      { kind: 'origin', origin: 'https://example.com/private' },
      { kind: 'application', bundle_id: 'other-app' },
      { kind: 'origin', origin: 'file:///etc' },
      { kind: 'origin', origin: 'https://example.com', wildcard: true },
    ])
      expect(() => controlPermission({ ...permission, target })).toThrow()
  })
  it('requires an explicit valid extent to broaden an external approval', () => {
    const request = {
      operation: 'approveExtension',
      id: 'root',
      approvalId: 'approval',
      decision: 'allow',
    }
    expect(apiRequest({ ...request, extent: 'target' }).body).toEqual({
      decision: 'allow',
      extent: 'target',
    })
    expect(apiRequest({ ...request, extent: 'capability' }).body).toEqual({
      decision: 'allow',
      extent: 'capability',
    })
    for (const extent of ['all', '', true, null])
      expect(() => apiRequest({ ...request, extent })).toThrow()
  })
})
