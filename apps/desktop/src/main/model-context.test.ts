import { afterEach, describe, expect, it, vi } from 'vitest'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { ModelInfo, ResolvedModelConfiguration } from '../shared/contracts'
import { refreshModelContext } from './model-context'
import { ModelVault } from './model-vault'
import { apiRequest, type ApiRequest } from './protocol'

const directories: string[] = []
const saved: ResolvedModelConfiguration = {
  protocol: 'anthropic_messages',
  base_url: 'https://fixture.example/v1',
  model: 'fixture',
  api_key: 'fixture-secret-only',
  context_window: 8192,
  max_output_tokens: 2048,
  reasoning_mode: 'standard',
  reasoning_effort: 'high',
  supports_images: true,
}
const cipher = {
  isEncryptionAvailable: () => true,
  encryptString: (value: string) => Buffer.from(value),
  decryptString: (value: Buffer) => value.toString(),
}
async function setup() {
  const directory = await mkdtemp(join(tmpdir(), 'model-context-test-'))
  directories.push(directory)
  const path = join(directory, 'vault')
  const vault = new ModelVault(path, cipher)
  await vault.save('old-model', saved)
  return { path, vault }
}
afterEach(async () => {
  await Promise.all(directories.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

describe('refresh saved model context', () => {
  it('accepts only an identifier over IPC, with no credential or capacity input', () => {
    expect(apiRequest({ operation: 'refreshModelContext', id: 'old-model' })).toEqual({
      path: '/api/models/old-model/context/refresh',
      method: 'POST',
    })
    expect(() => apiRequest({ operation: 'refreshModelContext', id: '../config' })).toThrow()
  })
  it('persists both profiles before migration, retains output/reasoning and returns no credentials', async () => {
    const { path, vault } = await setup()
    const requests: ApiRequest[] = []
    const model: ModelInfo = {
      id: 'new-model',
      name: saved.model,
      available: true,
      description: 'fixture',
      context_window: 1_000_000,
    }
    const result = await refreshModelContext(
      'old-model',
      {
        request: async (request) => {
          requests.push(request)
          if (request.path === '/api/runs') return []
          if (request.path === '/api/models') {
            expect(request.body).toEqual({ ...saved, context_window: 1_000_000 })
            return model
          }
          expect(request.path).toBe('/api/models/old-model/replace')
          expect(request.body).toEqual({ replacement_model_id: 'new-model' })
          expect(await new ModelVault(path, cipher).load()).toHaveLength(2)
          return model
        },
      },
      vault,
      async (input) => {
        expect(input.context_window).toBeUndefined()
        expect(input.api_key).toBe(saved.api_key)
        expect(input.max_output_tokens).toBe(2048)
        return undefined
      },
    )
    expect(result.limits).toMatchObject({
      context_window: 1_000_000,
      max_output_tokens: 2048,
      context_source: 'fallback',
    })
    expect(result.persisted).toBe(true)
    expect(JSON.stringify(result)).not.toContain(saved.api_key)
    expect(await new ModelVault(path, cipher).load()).toEqual([
      { ...saved, context_window: 1_000_000 },
    ])
    expect(requests).toHaveLength(3)
  })
  it('rejects a busy model before discovery or saving, retaining the original profile', async () => {
    const { vault } = await setup()
    for (const state of [
      { status: 'running' },
      { status: 'waiting_for_input', compacting: true },
    ]) {
      await expect(
        refreshModelContext(
          'old-model',
          {
            request: async (request) => {
              expect(request.path).toBe('/api/runs')
              return [{ model_id: 'old-model', ...state }]
            },
          },
          vault,
          async () => {
            throw new Error('discovery should not run')
          },
        ),
      ).rejects.toThrow('运行或压缩')
    }
    expect(vault.get('old-model')).toEqual(saved)
  })
  it('retains both durable connections if run migration fails', async () => {
    const { path, vault } = await setup()
    await expect(
      refreshModelContext(
        'old-model',
        {
          request: async (request) => {
            if (request.path === '/api/runs') return []
            if (request.path === '/api/models') return { id: 'new-model' }
            throw new Error('migration failed')
          },
        },
        vault,
        async () => ({ context_window: 262144 }),
      ),
    ).rejects.toThrow('migration failed')
    const restored = await new ModelVault(path, cipher).load()
    expect(restored).toEqual([saved, { ...saved, context_window: 262144 }])
  })

  it('reports the new configuration as persisted even when old-profile cleanup fails', async () => {
    const { path, vault } = await setup()
    vi.spyOn(vault, 'remove').mockResolvedValue(false)
    const result = await refreshModelContext(
      'old-model',
      {
        request: async (request) => (request.path === '/api/runs' ? [] : { id: 'new-model' }),
      },
      vault,
      async () => undefined,
    )
    expect(result.persisted).toBe(true)
    expect(result.warning).toContain('旧模型配置暂未清理')
    expect(await new ModelVault(path, cipher).load()).toHaveLength(2)
  })

  it('does not migrate any runs when the new connection cannot be persisted', async () => {
    const { vault } = await setup()
    vi.spyOn(vault, 'save').mockResolvedValue(false)
    const calls: string[] = []
    await expect(
      refreshModelContext(
        'old-model',
        {
          request: async (request) => {
            calls.push(request.path)
            return request.path === '/api/runs' ? [] : { id: 'new-model' }
          },
        },
        vault,
        async () => undefined,
      ),
    ).rejects.toThrow('现有会话尚未迁移')
    expect(calls).toEqual(['/api/runs', '/api/models'])
    expect(vault.get('old-model')).toEqual(saved)
  })
})
