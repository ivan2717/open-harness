import { mkdir, readFile, rename, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import type { ResolvedModelConfiguration } from '../shared/contracts'
import { identifier, resolvedModelConfiguration } from './protocol'

export interface CredentialCipher {
  isEncryptionAvailable(): boolean
  encryptString(value: string): Buffer
  decryptString(value: Buffer): string
}

/** Credentials never cross back to the renderer or enter run snapshots. */
export class ModelVault {
  private entries = new Map<string, ResolvedModelConfiguration>()
  private persistedEntries = new Map<string, ResolvedModelConfiguration>()
  private writable = true
  private writes: Promise<unknown> = Promise.resolve()

  constructor(
    private file: string,
    private cipher: CredentialCipher,
  ) {}

  /** Main-process access only; never send saved credentials through IPC. */
  get(id: string): ResolvedModelConfiguration | undefined {
    const input = this.entries.get(identifier(id))
    return input ? structuredClone(input) : undefined
  }

  /** Reuse an already durable replacement without registering or overwriting its credential. */
  findEquivalent(input: ResolvedModelConfiguration, excludeId: string): string | undefined {
    identifier(excludeId)
    const resolved = resolvedModelConfiguration(input)
    for (const [id, saved] of this.persistedEntries) {
      if (
        id !== excludeId &&
        isDeepStrictEqual(saved, resolved) &&
        isDeepStrictEqual(this.entries.get(id), saved)
      )
        return id
    }
    return undefined
  }

  /** Remove a superseded profile only after its replacement and run bindings are saved. */
  remove(id: string): Promise<boolean> {
    const remove = async (): Promise<boolean> => {
      const key = identifier(id)
      const next = new Map(this.entries)
      next.delete(key)
      if (!(await this.persist(next))) return false
      this.entries = next
      return true
    }
    const result = this.writes.then(remove, remove)
    this.writes = result.catch(() => undefined)
    return result
  }

  async load(): Promise<ResolvedModelConfiguration[]> {
    try {
      let encrypted: Buffer
      try {
        encrypted = await readFile(this.file)
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return []
        throw error
      }
      if (!this.cipher.isEncryptionAvailable()) throw new Error('Encryption unavailable')
      const saved: unknown = JSON.parse(this.cipher.decryptString(encrypted))
      if (!Array.isArray(saved)) throw new Error('Invalid model configuration')
      const entries = new Map<string, ResolvedModelConfiguration>()
      for (const item of saved) {
        if (!item || typeof item !== 'object') throw new Error('Invalid saved model')
        entries.set(identifier(item.id), resolvedModelConfiguration(item.input))
      }
      this.entries = entries
      this.persistedEntries = new Map(entries)
      return [...entries.values()]
    } catch {
      // Preserve an unreadable vault instead of replacing existing credentials.
      this.writable = false
      throw new Error(
        '无法解密已保存的模型配置。可以重新接入模型，本次配置仅在当前应用会话中生效。',
      )
    }
  }

  save(
    id: string,
    input: ResolvedModelConfiguration,
    options: { requirePersistence?: boolean } = {},
  ): Promise<boolean> {
    const save = async (): Promise<boolean> => {
      const next = new Map(this.entries)
      next.set(identifier(id), resolvedModelConfiguration(input))
      const persisted = await this.persist(next)
      if (persisted || !options.requirePersistence) this.entries = next
      return persisted
    }
    const result = this.writes.then(save, save)
    this.writes = result.catch(() => undefined)
    return result
  }

  private async persist(entries: Map<string, ResolvedModelConfiguration>): Promise<boolean> {
    if (!this.writable || !this.cipher.isEncryptionAvailable()) return false
    try {
      const encrypted = this.cipher.encryptString(
        JSON.stringify([...entries].map(([id, input]) => ({ id, input }))),
      )
      await mkdir(dirname(this.file), { recursive: true })
      const temporary = `${this.file}.tmp`
      await writeFile(temporary, encrypted, { mode: 0o600 })
      await rename(temporary, this.file)
      this.persistedEntries = new Map(entries)
      return true
    } catch {
      return false
    }
  }
}
