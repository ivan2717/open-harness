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
  it('keeps the memory setting across unrelated configuration saves', () => {
    expect(extensionConfiguration(fixture).llm_wiki_enabled).toBeUndefined()
    for (const enabled of [true, false]) {
      const request = apiRequest({
        operation: 'saveExtensions',
        config: { ...fixture, llm_wiki_enabled: enabled },
      })
      expect(request.body).toMatchObject({ llm_wiki_enabled: enabled })
    }
    for (const enabled of [null, 'false', 0, {}]) {
      expect(() => extensionConfiguration({ ...fixture, llm_wiki_enabled: enabled })).toThrow(
        '无效的记忆插件启用状态',
      )
    }
  })
  it('routes memory reads and the built-in toggle through bounded API operations', () => {
    expect(apiRequest({ operation: 'runMemory', id: 'run-1' })).toEqual({
      path: '/api/runs/run-1/memory',
      method: 'GET',
    })
    expect(
      apiRequest({ operation: 'setPluginEnabled', id: 'builtin.llm-wiki', enabled: false }),
    ).toEqual({
      path: '/api/plugins/builtin.llm-wiki/enabled',
      method: 'POST',
      body: { enabled: false },
    })
    for (const id of ['../private', 'run-1?other=true', '', null])
      expect(() => apiRequest({ operation: 'runMemory', id })).toThrow()
  })
  it('maps read-only skill discovery and keeps the workspace access check explicit', () => {
    expect(apiRequest({ operation: 'skillCatalog' })).toEqual({
      path: '/api/extensions/skills',
      method: 'GET',
    })
    const path = '/Users/test/项目 & notes'
    expect(apiRequest({ operation: 'skillCatalog', workspacePath: path })).toEqual({
      path: `/api/extensions/skills?workspace_path=${encodeURIComponent(path)}`,
      method: 'GET',
      workspacePath: path,
    })
    for (const path of [
      null,
      42,
      '',
      'relative/path',
      '/tmp/\0private',
      '/tmp/\nprivate',
      `/${'x'.repeat(4096)}`,
    ]) {
      expect(() => apiRequest({ operation: 'skillCatalog', workspacePath: path })).toThrow()
    }
  })
  it('maps bounded settings and run-scoped approvals through the fixed API', () => {
    expect(apiRequest({ operation: 'saveExtensions', config: fixture }).body).toEqual(
      extensionConfiguration(fixture),
    )
    expect(
      apiRequest({
        operation: 'approveExtension',
        id: 'run-1',
        approvalId: 'mcp-1',
        decision: 'allow',
      }),
    ).toEqual({
      path: '/api/runs/run-1/extensions/approvals/mcp-1',
      method: 'POST',
      body: { decision: 'allow' },
    })
    expect(apiRequest({ operation: 'refreshRunSkills', id: 'run-1' }).path).toBe(
      '/api/runs/run-1/extensions/refresh',
    )
  })
  it('preserves the explicit control decision and rejects obsolete or ambiguous payloads', () => {
    for (const decision of ['allow', 'always', 'cancel']) {
      expect(
        apiRequest({
          operation: 'approveExtension',
          id: 'run-1',
          approvalId: 'mcp-2',
          decision,
        }),
      ).toEqual({
        path: '/api/runs/run-1/extensions/approvals/mcp-2',
        method: 'POST',
        body: { decision },
      })
    }
    for (const decision of [undefined, null, true, false, 'once', 'session', '', {}, []])
      expect(() =>
        apiRequest({
          operation: 'approveExtension',
          id: 'run-1',
          approvalId: 'mcp-2',
          decision,
        }),
      ).toThrow('Invalid approval decision')
    expect(() =>
      apiRequest({
        operation: 'approveExtension',
        id: 'run-1',
        approvalId: 'mcp-2',
        allow: true,
        session: true,
      }),
    ).toThrow('Invalid approval decision')
    for (const approvalId of ['../other', 'mcp-2?allow=true', '', null])
      expect(() =>
        apiRequest({
          operation: 'approveExtension',
          id: 'run-1',
          approvalId,
          decision: 'always',
        }),
      ).toThrow()
  })
  it('revokes a saved capability only through its fixed server endpoint', () => {
    expect(
      apiRequest({ operation: 'revokeControlPermission', serverId: 'plugin_chrome_use_chrome' }),
    ).toEqual({
      path: '/api/extensions/control-permissions/plugin_chrome_use_chrome',
      method: 'DELETE',
    })
    for (const serverId of ['../other', 'chrome?all=true', '', null])
      expect(() => apiRequest({ operation: 'revokeControlPermission', serverId })).toThrow()
  })
  it('revokes only the named run through a fixed DELETE endpoint', () => {
    expect(apiRequest({ operation: 'revokeControl', id: 'run-1' })).toEqual({
      path: '/api/runs/run-1/extensions/control',
      method: 'DELETE',
    })
    expect(
      apiRequest({
        operation: 'revokeControl',
        id: 'run-1',
        path: 'https://example.com/all',
        method: 'POST',
        body: { scope: 'global' },
      }),
    ).toEqual({ path: '/api/runs/run-1/extensions/control', method: 'DELETE' })
    for (const id of [
      '../other',
      'run-1/../other',
      'run-1?scope=all',
      'https://example.com',
      '',
      null,
    ])
      expect(() => apiRequest({ operation: 'revokeControl', id })).toThrow()
  })
  it('preserves qualified plugin skill identifiers at their full bound', () => {
    const id = `${'p'.repeat(64)}:${'s'.repeat(64)}`
    expect(
      extensionConfiguration({ ...fixture, disabled_skill_ids: [id] }).disabled_skill_ids,
    ).toEqual([id])
    expect(() => extensionConfiguration({ ...fixture, disabled_skill_ids: [`${id}s`] })).toThrow()
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
