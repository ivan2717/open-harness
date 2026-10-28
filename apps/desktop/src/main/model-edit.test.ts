import { afterEach, describe, expect, it, vi } from 'vitest'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type {
  ConfigureModelInput,
  ModelInfo,
  ResolvedModelConfiguration,
  UpdateModelInput,
} from '../shared/contracts'
import { getModelConfiguration, updateModelConfiguration } from './model-edit'
import { refreshModelContext } from './model-context'
import { ModelVault } from './model-vault'
import { apiRequest, type ApiRequest } from './protocol'

const directories: string[] = []
const saved: ResolvedModelConfiguration = {
  protocol: 'responses',
  base_url: 'https://fixture.example/v1',
  model: 'fixture',
  api_key: 'fixture-only-secret',
  context_window: 128000,
  max_output_tokens: 8192,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
  reasoning_default: 'high',
  reasoning_capability: {
    source: 'manual',
    adapter: 'effort',
    options: [{ id: 'high', effort: 'high' }],
  },
  supports_images: false,
}
const cipher = {
  isEncryptionAvailable: () => true,
  encryptString: (value: string) => Buffer.from(value),
  decryptString: (value: Buffer) => value.toString(),
}
const model: ModelInfo = {
  id: 'new-model',
  name: 'fixture',
  available: true,
  description: 'fixture',
  context_window: 256000,
}
async function setup() {
  const directory = await mkdtemp(join(tmpdir(), 'model-edit-test-'))
  directories.push(directory)
  const path = join(directory, 'vault')
  const vault = new ModelVault(path, cipher)
  await vault.save('old-model', saved)
  const { api_key: _secret, ...input } = saved
  return { path, vault, input: input as UpdateModelInput }
}
afterEach(async () => {
  await Promise.all(directories.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

describe('edit a saved model connection', () => {
  it('returns editable fields but never the credential or a mutable vault reference', async () => {
    const { vault, input } = await setup()
    const result = getModelConfiguration('old-model', vault)
    expect(result).toEqual({ ...input, id: 'old-model', has_api_key: true })
    expect(JSON.stringify(result)).not.toContain(saved.api_key)
    result.reasoning_capability!.options[0].effort = 'low'
    expect(vault.get('old-model')).toEqual(saved)
    expect(() => getModelConfiguration('missing', vault)).toThrow('配置不可用')
  })

  it.each([undefined, '', '   '])(
    'keeps a blank key (%s), preserves explicit settings and persists before migrating',
    async (api_key) => {
      const { path, vault, input } = await setup()
      const updated = { ...input, api_key, context_window: 256000, supports_images: true }
      const discover = vi.fn(async (_input: ConfigureModelInput) => ({
        capacity: { context_window: 1000000 },
      }))
      const request = vi.fn(async (request: ApiRequest) => {
        if (request.path === '/api/runs') return []
        if (request.path === '/api/models') {
          expect(request.body).toEqual({
            ...saved,
            context_window: 256000,
            supports_images: true,
            edit_source_id: 'old-model',
          })
          return model
        }
        expect(request).toEqual({
          path: '/api/models/old-model/replace',
          method: 'POST',
          body: { replacement_model_id: model.id, allow_configuration_change: true },
        })
        expect(await new ModelVault(path, cipher).load()).toHaveLength(2)
        return model
      })
      const result = await updateModelConfiguration(
        'old-model',
        updated,
        { request },
        vault,
        discover,
      )
      expect(discover.mock.calls[0][0]).toMatchObject({ api_key: saved.api_key })
      expect(JSON.stringify(result)).not.toContain(saved.api_key)
      expect(result.persisted).toBe(true)
      expect(await new ModelVault(path, cipher).load()).toEqual([
        { ...saved, context_window: 256000, supports_images: true },
      ])
    },
  )

  it('blocks an origin change without an explicit key before discovery or backend requests', async () => {
    const { vault, input } = await setup()
    const request = vi.fn()
    const discover = vi.fn()
    for (const base_url of ['https://other.example/v1', 'https://fixture.example:8443/v1']) {
      await expect(
        updateModelConfiguration('old-model', { ...input, base_url }, { request }, vault, discover),
      ).rejects.toThrow('API key')
    }
    expect(request).not.toHaveBeenCalled()
    expect(discover).not.toHaveBeenCalled()
    expect(vault.get('old-model')).toEqual(saved)
  })

  it('uses the explicitly supplied replacement key for a new service', async () => {
    const { vault, input } = await setup()
    const update = { ...input, base_url: 'https://other.example/v1', api_key: 'new-service-secret' }
    const discover = vi.fn(async (_input: ConfigureModelInput) => undefined)
    await updateModelConfiguration(
      'old-model',
      update,
      { request: async (request) => (request.path === '/api/runs' ? [] : model) },
      vault,
      discover,
    )
    expect(discover.mock.calls[0][0]).toMatchObject({
      api_key: update.api_key,
      base_url: update.base_url,
    })
    expect(vault.get(model.id)?.api_key).toBe(update.api_key)
  })

  it('rejects running and compacting profiles before registering or persisting changes', async () => {
    const { vault, input } = await setup()
    const discover = vi.fn()
    for (const state of [{ status: 'running' }, { status: 'completed', compacting: true }]) {
      const request = vi.fn(async () => [{ model_id: 'old-model', ...state }])
      await expect(
        updateModelConfiguration('old-model', input, { request }, vault, discover),
      ).rejects.toThrow('运行或压缩')
      expect(request).toHaveBeenCalledExactlyOnceWith({ path: '/api/runs', method: 'GET' })
    }
    expect(discover).not.toHaveBeenCalled()
    expect(vault.get('old-model')).toEqual(saved)
  })

  it('keeps both durable profiles when run migration fails', async () => {
    const { path, vault, input } = await setup()
    await expect(
      updateModelConfiguration(
        'old-model',
        { ...input, context_window: 256000 },
        {
          request: async (request) => {
            if (request.path === '/api/runs') return []
            if (request.path === '/api/models') return model
            throw new Error('fixture migration failure')
          },
        },
        vault,
        async () => undefined,
      ),
    ).rejects.toThrow('migration failure')
    const reopened = new ModelVault(path, cipher)
    expect(await reopened.load()).toEqual([saved, { ...saved, context_window: 256000 }])
    const save = vi.spyOn(reopened, 'save')
    const retry = vi.fn(async (request: ApiRequest) => {
      if (request.path === '/api/runs') return []
      if (request.path === '/api/config') return { models: [model] }
      expect(request.path).toBe('/api/models/old-model/replace')
      return model
    })
    await expect(
      updateModelConfiguration(
        'old-model',
        { ...input, context_window: 256000 },
        { request: retry },
        reopened,
        async () => undefined,
      ),
    ).resolves.toMatchObject({ persisted: true, model })
    expect(retry.mock.calls.map(([request]) => request.path)).toEqual([
      '/api/runs',
      '/api/config',
      '/api/models/old-model/replace',
    ])
    expect(save).not.toHaveBeenCalled()
    expect(await new ModelVault(path, cipher).load()).toEqual([
      { ...saved, context_window: 256000 },
    ])
  })

  it('rolls back an in-place key rotation if persistence fails and never migrates', async () => {
    const { vault, input } = await setup()
    vi.spyOn(vault, 'save').mockResolvedValue(false)
    const request = vi.fn(async (request: ApiRequest) =>
      request.path === '/api/runs' ? [] : { ...model, id: 'old-model' },
    )
    await expect(
      updateModelConfiguration(
        'old-model',
        { ...input, api_key: 'new-key' },
        { request },
        vault,
        async () => undefined,
      ),
    ).rejects.toThrow('无法保存')
    expect(request).toHaveBeenLastCalledWith({
      path: '/api/models',
      method: 'POST',
      body: { ...saved, edit_source_id: 'old-model' },
    })
    expect(request.mock.calls.map(([call]) => call.path)).toEqual([
      '/api/runs',
      '/api/models',
      '/api/models',
    ])
    expect(vault.get('old-model')).toEqual(saved)
  })

  it('shares the source-profile guard with refresh and releases it after success', async () => {
    const { vault, input } = await setup()
    let release!: () => void
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    let started!: () => void
    const ready = new Promise<void>((resolve) => {
      started = resolve
    })
    const backend = {
      request: async (request: ApiRequest) =>
        request.path === '/api/runs' ? [] : { ...model, id: 'old-model' },
    }
    const editing = updateModelConfiguration('old-model', input, backend, vault, async () => {
      started()
      await gate
      return undefined
    })
    await ready
    await expect(
      refreshModelContext('old-model', backend, vault, async () => undefined),
    ).rejects.toThrow('正在更新')
    await expect(
      updateModelConfiguration('old-model', input, backend, vault, async () => undefined),
    ).rejects.toThrow('正在更新')
    release()
    await editing
    await expect(
      updateModelConfiguration('old-model', input, backend, vault, async () => undefined),
    ).resolves.toMatchObject({ persisted: true })
  })

  it('validates edit requests without embedding a retained-key sentinel into IPC bodies', () => {
    const { api_key: _key, ...input } = saved
    expect(apiRequest({ operation: 'getModelConfiguration', id: 'model' })).toEqual({
      path: '/api/models/model/configuration',
      method: 'GET',
    })
    expect(
      apiRequest({
        operation: 'updateModelConfiguration',
        id: 'model',
        input: { ...input, api_key: '' },
      }),
    ).toEqual({ path: '/api/models/model/configuration', method: 'PATCH', body: input })
    for (const changes of [
      { api_key: null },
      { api_key: 'a'.repeat(4097) },
      { base_url: 'http://remote.example' },
      { supports_images: 'yes' },
    ])
      expect(() =>
        apiRequest({
          operation: 'updateModelConfiguration',
          id: 'model',
          input: { ...input, ...changes },
        }),
      ).toThrow()
    expect(() => apiRequest({ operation: 'getModelConfiguration', id: '../model' })).toThrow()
  })
})
