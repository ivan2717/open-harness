import { afterEach, describe, expect, it } from 'vitest'
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { ModelVault, type CredentialCipher } from './model-vault'
import { apiRequest, modelConfiguration, resolvedModelConfiguration } from './protocol'
import type { ResolvedModelConfiguration } from '../shared/contracts'

const input: ResolvedModelConfiguration = {
  protocol: 'responses',
  base_url: 'https://example.com/v1',
  api_key: 'test-secret',
  model: 'test-model',
  context_window: 1_000_000,
  max_output_tokens: 8192,
  reasoning_mode: 'standard',
  reasoning_effort: 'medium',
}
const cipher: CredentialCipher = {
  isEncryptionAvailable: () => true,
  encryptString: (text) => Buffer.from(Buffer.from(text).toString('base64')),
  decryptString: (bytes) => Buffer.from(bytes.toString(), 'base64').toString(),
}
const directories: string[] = []
async function location() {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-vault-'))
  directories.push(directory)
  return join(directory, 'models.encrypted')
}
afterEach(async () => {
  await Promise.all(directories.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

describe('model credential storage', () => {
  it('preserves independent capability mapping and default across restart', async () => {
    const file = await location()
    const configured: ResolvedModelConfiguration = {
      ...input,
      reasoning_mode: 'off',
      reasoning_default: 'thorough',
      reasoning_capability: {
        source: 'manual',
        adapter: 'effort',
        options: [{ id: 'thorough', label: '深入', effort: 'xhigh' }],
      },
    }
    const vault = new ModelVault(file, cipher)
    await vault.save('model', configured)
    const copy = vault.get('model')!
    copy.reasoning_capability!.options[0].effort = 'low'
    expect(vault.get('model')).toEqual(configured)
    expect(await new ModelVault(file, cipher).load()).toEqual([configured])
  })
  it('stores only cipher output, serializes concurrent saves, and restores multiple configurations', async () => {
    const file = await location()
    const vault = new ModelVault(file, cipher)
    await Promise.all([
      vault.save('model-a', input),
      vault.save('model-b', { ...input, model: 'other' }),
    ])
    expect((await readFile(file)).toString()).not.toContain(input.api_key)
    expect((await stat(file)).mode & 0o777).toBe(0o600)
    expect(await new ModelVault(file, cipher).load()).toEqual([input, { ...input, model: 'other' }])
  })
  it('does not write plaintext when OS encryption is unavailable', async () => {
    const file = await location()
    const vault = new ModelVault(file, { ...cipher, isEncryptionAvailable: () => false })
    expect(await vault.save('model', input)).toBe(false)
    await expect(readFile(file)).rejects.toMatchObject({ code: 'ENOENT' })
  })
  it('keeps credentials internal and removes superseded profiles durably', async () => {
    const file = await location()
    const vault = new ModelVault(file, cipher)
    await vault.save('old-model', { ...input, context_window: 8192, max_output_tokens: 2048 })
    const copy = vault.get('old-model')!
    copy.context_window = 1_000_000
    expect(vault.get('old-model')?.context_window).toBe(8192)
    await vault.save('new-model', copy)
    expect(await vault.remove('old-model')).toBe(true)
    expect(vault.get('old-model')).toBeUndefined()
    const reopened = new ModelVault(file, cipher)
    expect(await reopened.load()).toEqual([copy])
    expect(reopened.get('new-model')).toEqual(copy)
    expect(reopened.get('old-model')).toBeUndefined()
  })
  it('preserves an unreadable existing vault', async () => {
    const file = await location()
    await writeFile(file, 'unreadable')
    const vault = new ModelVault(file, cipher)
    await expect(vault.load()).rejects.toThrow('无法解密')
    expect(await vault.save('model', input)).toBe(false)
    expect(await readFile(file, 'utf8')).toBe('unreadable')
  })
  it('restores final numeric capacities unchanged and rejects unresolved saved models', async () => {
    const file = await location()
    const resolved = { ...input, context_window: 32768, max_output_tokens: 4096 }
    await new ModelVault(file, cipher).save('resolved-model', resolved)
    expect(await new ModelVault(file, cipher).load()).toEqual([resolved])
    const unresolved = { ...input, context_window: undefined }
    await writeFile(
      file,
      cipher.encryptString(JSON.stringify([{ id: 'model', input: unresolved }])),
    )
    await expect(new ModelVault(file, cipher).load()).rejects.toThrow('无法解密')
  })
})

describe('provider IPC validation', () => {
  it('validates configured defaults after capability discovery without conflating default and off', () => {
    const capability = {
      source: 'manual',
      adapter: 'deepseek',
      options: [
        { id: 'off', enabled: false },
        { id: 'thorough', effort: 'max' },
      ],
    }
    const configured = {
      ...input,
      protocol: 'chat_completions',
      reasoning_mode: 'off',
      reasoning_capability: capability,
    }
    expect(
      resolvedModelConfiguration({ ...configured, reasoning_default: null }).reasoning_default,
    ).toBeNull()
    expect(
      resolvedModelConfiguration({ ...configured, reasoning_default: 'off' }).reasoning_default,
    ).toBe('off')
    expect(
      resolvedModelConfiguration({ ...configured, reasoning_default: 'thorough' })
        .reasoning_default,
    ).toBe('thorough')
    expect(() => resolvedModelConfiguration({ ...configured, reasoning_default: 'high' })).toThrow(
      '不受支持',
    )
    expect(() => modelConfiguration({ ...configured, reasoning_mode: 'standard' })).toThrow()
    expect(
      modelConfiguration({ ...input, reasoning_mode: 'off', reasoning_default: 'xhigh' })
        .reasoning_default,
    ).toBe('xhigh')
    expect(() =>
      resolvedModelConfiguration({ ...input, reasoning_mode: 'off', reasoning_default: 'xhigh' }),
    ).toThrow('声明模型能力')
    expect(() =>
      resolvedModelConfiguration({
        ...input,
        protocol: 'anthropic_messages',
        reasoning_mode: 'off',
        reasoning_default: 'large',
        reasoning_capability: {
          source: 'manual',
          adapter: 'anthropic_budget',
          options: [{ id: 'large', budget_tokens: 8192 }],
        },
      }),
    ).toThrow('输出上限')
  })
  it('accepts explicit protocol settings and only fixed maintenance routes', () => {
    expect(modelConfiguration(input)).toEqual(input)
    expect(apiRequest({ operation: 'compactRun', id: 'run-a' }).path).toBe(
      '/api/runs/run-a/compact',
    )
    expect(apiRequest({ operation: 'cancelCompaction', id: 'run-a' }).path).toBe(
      '/api/runs/run-a/compact/cancel',
    )
  })
  it('rejects credentials embedded in URLs, invalid capacity and incompatible reasoning', () => {
    for (const change of [
      { base_url: 'https://user:secret@example.com/v1' },
      { base_url: 'http://example.com/v1' },
      { base_url: 'https://example.com/v1?api_key=secret' },
      { context_window: Number.NaN },
      { context_window: 4096 },
      { max_output_tokens: 0 },
      { reasoning_mode: 'deepseek' },
    ])
      expect(() => modelConfiguration({ ...input, ...change })).toThrow()
    expect(
      modelConfiguration({ ...input, base_url: 'http://127.0.0.1:8100/v1' }).base_url,
    ).toContain('127.0.0.1')
  })
  it('accepts omitted overrides but requires resolved capacities at the persistence boundary', () => {
    const automatic = { ...input, context_window: undefined, max_output_tokens: undefined }
    expect(modelConfiguration(automatic)).toEqual(automatic)
    expect(modelConfiguration({ ...automatic, context_window: 8192 }).context_window).toBe(8192)
    expect(() => resolvedModelConfiguration(automatic)).toThrow('resolved')
    for (const change of [
      { context_window: null },
      { context_window: '8192' },
      { context_window: 8192.5 },
      { max_output_tokens: null },
      { max_output_tokens: Number.POSITIVE_INFINITY },
      { max_output_tokens: 8_000_001 },
      { context_window: 1024, max_output_tokens: 960 },
    ])
      expect(() => modelConfiguration({ ...automatic, ...change })).toThrow()
  })
})
