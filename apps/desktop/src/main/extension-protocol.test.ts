import { describe, expect, it } from 'vitest'
import { extensionConfiguration } from './extension-protocol'
import { apiRequest } from './protocol'

const fixture = {
  mcp_servers: [
    {
      id: 'fixture',
      enabled: true,
      transport: {
        type: 'stdio',
        command: '/usr/bin/node',
        args: ['server.mjs'],
        env_vars: ['MCP_TOKEN'],
      },
    },
  ],
  disabled_skill_ids: ['review'],
}
describe('extension bridge validation', () => {
  it('maps bounded settings and run-scoped approvals through the fixed API', () => {
    expect(apiRequest({ operation: 'saveExtensions', config: fixture }).body).toEqual(
      extensionConfiguration(fixture),
    )
    expect(
      apiRequest({ operation: 'approveExtension', id: 'run-1', approvalId: 'mcp-1', allow: true }),
    ).toEqual({
      path: '/api/runs/run-1/extensions/approvals/mcp-1',
      method: 'POST',
      body: { allow: true },
    })
    expect(apiRequest({ operation: 'refreshRunSkills', id: 'run-1' }).path).toBe(
      '/api/runs/run-1/extensions/refresh',
    )
  })
  it('rejects secret values, arbitrary paths, duplicate servers and insecure remote URLs', () => {
    expect(() =>
      extensionConfiguration({
        ...fixture,
        mcp_servers: [fixture.mcp_servers[0], fixture.mcp_servers[0]],
      }),
    ).toThrow()
    expect(() =>
      extensionConfiguration({
        ...fixture,
        mcp_servers: [
          {
            ...fixture.mcp_servers[0],
            transport: { ...fixture.mcp_servers[0].transport, env_vars: ['TOKEN=secret'] },
          },
        ],
      }),
    ).toThrow()
    for (const url of [
      'file:///tmp/a',
      'http://remote.example/mcp',
      'https://user:secret@example.com/mcp',
    ]) {
      expect(() =>
        extensionConfiguration({
          ...fixture,
          mcp_servers: [{ id: 'fixture', enabled: true, transport: { type: 'http', url } }],
        }),
      ).toThrow()
    }
    expect(() => apiRequest({ operation: 'testMcp', id: '../models' })).toThrow()
    expect(() =>
      apiRequest({
        operation: 'approveExtension',
        id: 'run-1',
        approvalId: 'mcp-1',
        allow: 'true',
      }),
    ).toThrow()
  })
})
