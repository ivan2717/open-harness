import type { ConfigureModelInput } from '../shared/contracts'
import { MODEL_TOKEN_LIMIT, type ModelCapacity } from '../shared/model-limits'

const MAX_METADATA_BYTES = 256 * 1024

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined
}

function tokenLimit(value: unknown, minimum: number): number | undefined {
  return typeof value === 'number' &&
    Number.isSafeInteger(value) &&
    value >= minimum &&
    value <= MODEL_TOKEN_LIMIT
    ? value
    : undefined
}

async function boundedJson(response: Response): Promise<unknown> {
  const length = Number(response.headers.get('content-length'))
  if (length > MAX_METADATA_BYTES || !response.body) return undefined
  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_METADATA_BYTES) return undefined
      chunks.push(value)
    }
    return JSON.parse(Buffer.concat(chunks).toString('utf8')) as unknown
  } finally {
    void reader.cancel().catch(() => undefined)
  }
}

/** Optional discovery only: one bounded GET, never a generation or token-counting request. */
export async function fetchModelCapacity(
  input: ConfigureModelInput,
  request: typeof fetch = fetch,
): Promise<ModelCapacity | undefined> {
  if (input.context_window !== undefined && input.max_output_tokens !== undefined) return undefined
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 2000)
  try {
    const base = new URL(input.base_url)
    const local = ['localhost', '127.0.0.1', '[::1]'].includes(base.hostname)
    if (
      (base.protocol !== 'https:' && !(base.protocol === 'http:' && local)) ||
      base.username ||
      base.password ||
      base.search ||
      base.hash
    )
      return undefined
    const path = base.pathname.replace(/\/+$/, '')
    // OpenAI's Models API documents identity/ownership only, without token capacities.
    if (base.origin === 'https://api.openai.com' && path === '/v1') return undefined
    const deepseek =
      base.origin === 'https://api.deepseek.com' &&
      (input.protocol === 'anthropic_messages'
        ? ['/anthropic', '/anthropic/v1'].includes(path)
        : ['', '/v1'].includes(path))
    const anthropic = input.protocol === 'anthropic_messages' && !deepseek
    if (anthropic && ['.', '..'].includes(input.model)) return undefined
    const url = deepseek
      ? `${base.origin}/models`
      : `${base.origin}${path}/models${anthropic ? `/${encodeURIComponent(input.model)}` : ''}`
    const headers: Record<string, string> = { Accept: 'application/json' }
    if (anthropic) {
      headers['x-api-key'] = input.api_key
      headers['anthropic-version'] = '2023-06-01'
    } else headers.Authorization = `Bearer ${input.api_key}`
    const response = await request(url, {
      method: 'GET',
      headers,
      signal: controller.signal,
      redirect: 'error',
      credentials: 'omit',
    })
    if (!response.ok) return undefined
    const body = record(await boundedJson(response))
    const item = anthropic
      ? body
      : Array.isArray(body?.data)
        ? body.data.map(record).find((entry) => entry?.id === input.model)
        : undefined
    // Retrieval can resolve an Anthropic alias; list discovery always requires the exact ID.
    if (!item || typeof item.id !== 'string' || !item.id) return undefined
    const context_window = tokenLimit(anthropic ? item.max_input_tokens : item.context_window, 1024)
    const max_output_tokens = tokenLimit(anthropic ? item.max_tokens : item.max_output_tokens, 64)
    return context_window !== undefined || max_output_tokens !== undefined
      ? { context_window, max_output_tokens }
      : undefined
  } catch {
    // Discovery must not block offline setup or expose provider errors/credentials.
    return undefined
  } finally {
    clearTimeout(timeout)
    controller.abort()
  }
}
