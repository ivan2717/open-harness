import type { ConfigureModelInput } from '../shared/contracts'
import { MODEL_TOKEN_LIMIT, type ModelCapacity } from '../shared/model-limits'
import {
  officialReasoningProvider,
  validReasoningId,
  validateReasoningCapability,
  type ReasoningCapability,
  type ReasoningProvider,
} from '../shared/model-reasoning'

export interface ModelMetadata {
  capacity?: ModelCapacity
  reasoning?: ReasoningCapability
}
const MAX_METADATA_BYTES = 256 * 1024

type Protocol = ConfigureModelInput['protocol']
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
export async function fetchModelMetadata(
  input: ConfigureModelInput,
  request: typeof fetch = fetch,
): Promise<ModelMetadata | undefined> {
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
    const provider = officialReasoningProvider(input)
    // OpenAI documents identity/ownership only. A preset remains editable without this GET.
    if (provider === 'openai') return undefined
    const path = base.pathname.replace(/\/+$/, '')
    const model = input.model.trim()
    const anthropic =
      input.protocol === 'anthropic_messages' &&
      !['deepseek', 'kimi', 'openrouter', 'qwen'].includes(provider ?? '')
    if (['.', '..'].includes(model)) return undefined
    let url = `${base.origin}${path}/models${anthropic ? `/${encodeURIComponent(model)}` : ''}`
    const headers: Record<string, string> = { Accept: 'application/json' }
    if (provider === 'gemini') {
      const modelName = model.replace(/^models\//, '')
      if (!modelName || modelName.includes('/') || ['.', '..'].includes(modelName)) return undefined
      url = `${base.origin}/v1beta/models/${encodeURIComponent(modelName)}`
      headers['x-goog-api-key'] = input.api_key
    } else if (provider === 'qwen') {
      url = `${base.origin}/api/v1/models?model=${encodeURIComponent(model)}`
      headers.Authorization = `Bearer ${input.api_key}`
    } else if (provider === 'openrouter') {
      const parts = model.split('/')
      if (parts.length !== 2 || parts.some((part) => !part || ['.', '..'].includes(part)))
        return undefined
      url = `${base.origin}/api/v1/model/${parts.map(encodeURIComponent).join('/')}`
      headers.Authorization = `Bearer ${input.api_key}`
    } else if (anthropic) {
      if (provider === 'anthropic') url = `${base.origin}/v1/models/${encodeURIComponent(model)}`
      headers['x-api-key'] = input.api_key
      // Metadata follows the current official schema; generation keeps its own API version.
      headers['anthropic-version'] = provider === 'anthropic' ? '2026-10-21' : '2023-06-01'
    } else {
      if (provider === 'deepseek') url = `${base.origin}/models`
      else if (provider === 'kimi' || provider === 'xai') url = `${base.origin}/v1/models`
      headers.Authorization = `Bearer ${input.api_key}`
    }
    const response = await request(url, {
      method: 'GET',
      headers,
      signal: controller.signal,
      redirect: 'error',
      credentials: 'omit',
    })
    if (!response.ok) return undefined
    const body = record(await boundedJson(response))
    let item: Record<string, unknown> | undefined
    if (provider === 'qwen') {
      const models = record(body?.output)?.models
      item = Array.isArray(models)
        ? models.map(record).find((entry) => entry?.model === model)
        : undefined
    } else if (provider === 'openrouter') item = record(body?.data)
    else if (provider === 'gemini')
      item = body?.name === `models/${model.replace(/^models\//, '')}` ? body : undefined
    else if (anthropic) item = body
    else if (Array.isArray(body?.data))
      item = body.data.map(record).find((entry) => entry?.id === model)
    // Official single-model endpoints may resolve aliases. Lists must match the requested ID.
    if (
      !item ||
      (provider !== 'gemini' && provider !== 'qwen' && (typeof item.id !== 'string' || !item.id))
    )
      return undefined
    const limits = provider === 'qwen' ? record(item.model_info) : item
    const context_window = tokenLimit(
      provider === 'gemini'
        ? item.inputTokenLimit
        : provider === 'qwen'
          ? (limits?.context_window ?? limits?.max_input_tokens)
          : anthropic
            ? item.max_input_tokens
            : (item.context_window ?? item.context_length),
      1024,
    )
    const max_output_tokens = tokenLimit(
      provider === 'gemini'
        ? item.outputTokenLimit
        : provider === 'openrouter'
          ? record(item.top_provider)?.max_completion_tokens
          : anthropic
            ? item.max_tokens
            : limits?.max_output_tokens,
      64,
    )
    const capacity =
      context_window !== undefined || max_output_tokens !== undefined
        ? { context_window, max_output_tokens }
        : undefined
    const reasoning = modelReasoning(item, input.protocol, provider)
    return capacity || reasoning ? { capacity, reasoning } : undefined
  } catch {
    // Offline setup and provider errors never prevent saving or expose credentials.
    return undefined
  } finally {
    clearTimeout(timeout)
    controller.abort()
  }
}

/** Capacity-only refresh deliberately leaves the connection's reasoning contract unchanged. */
export async function fetchModelCapacity(
  input: ConfigureModelInput,
  request: typeof fetch = fetch,
): Promise<ModelCapacity | undefined> {
  if (input.context_window !== undefined && input.max_output_tokens !== undefined) return undefined
  return (await fetchModelMetadata(input, request))?.capacity
}
function levels(value: unknown, allowEmpty = false): string[] | undefined {
  return Array.isArray(value) &&
    (allowEmpty || value.length > 0) &&
    value.length <= 31 &&
    value.every(validReasoningId) &&
    new Set(value).size === value.length
    ? value
    : undefined
}
function supported(value: unknown): boolean | undefined {
  const result = record(value)?.supported
  return typeof result === 'boolean' ? result : undefined
}
function malformedSupport(value: unknown): boolean {
  return value !== undefined && value !== null && supported(value) === undefined
}
function effortCapability(efforts: string[], protocol: Protocol): ReasoningCapability {
  return {
    source: 'metadata',
    adapter: protocol === 'anthropic_messages' ? 'messages_effort_only' : 'effort',
    options: efforts.map((id) => ({ id, effort: id })),
  }
}
function modelReasoning(
  item: Record<string, unknown>,
  protocol: Protocol,
  provider: ReasoningProvider | undefined,
): ReasoningCapability | undefined {
  try {
    // Relays may explicitly declare their wire mapping. Parameter names alone aren't levels.
    const declared = record(item.reasoning_capability)
    if (declared) return validateReasoningCapability({ ...declared, source: 'metadata' }, protocol)
    if (provider === 'deepseek') {
      const values = levels(record(item.effort)?.supported_levels)
      if (!values || values.includes('off') || values.includes('none')) return undefined
      return validateReasoningCapability(
        {
          source: 'metadata',
          adapter:
            protocol === 'chat_completions'
              ? 'deepseek'
              : protocol === 'responses'
                ? 'effort'
                : 'messages_effort',
          options: [
            protocol === 'responses'
              ? { id: 'none', effort: 'none' }
              : { id: 'off', enabled: false },
            ...values.map((id) => ({ id, effort: id })),
          ],
        },
        protocol,
      )
    }
    if (provider === 'anthropic' && protocol === 'anthropic_messages') {
      const capabilities = record(item.capabilities)
      const effort = record(capabilities?.effort)
      const thinking = record(capabilities?.thinking)
      const types = record(thinking?.types)
      const effortNames = ['low', 'medium', 'high', 'xhigh', 'max']
      const thinkingTypes = ['adaptive', 'enabled', 'disabled', 'between_tools']
      if (
        malformedSupport(capabilities?.effort) ||
        malformedSupport(capabilities?.thinking) ||
        effortNames.some((id) => malformedSupport(effort?.[id])) ||
        thinkingTypes.some((id) => malformedSupport(types?.[id]))
      )
        return undefined
      // A false aggregate cannot consistently advertise an enabled sub-capability.
      if (
        (effort?.supported === false && effortNames.some((id) => supported(effort[id]) === true)) ||
        (thinking?.supported === false &&
          ['adaptive', 'enabled', 'between_tools'].some((id) => supported(types?.[id]) === true))
      )
        return undefined
      if (effort?.supported === true) {
        const values = effortNames.filter((id) => supported(effort[id]) === true)
        if (!values.length) return undefined
        const adaptive = supported(types?.adaptive) === true
        return validateReasoningCapability(
          {
            source: 'metadata',
            adapter: adaptive ? 'anthropic_adaptive' : 'messages_effort_only',
            options: [
              ...(adaptive && supported(types?.disabled) === true
                ? [{ id: 'off', enabled: false }]
                : []),
              ...values.map((id) => ({ id, effort: id })),
            ],
          },
          protocol,
        )
      }
      if (supported(types?.enabled) === true)
        return validateReasoningCapability(
          {
            source: 'preset',
            adapter: 'anthropic_budget',
            options: [
              ...(supported(types?.disabled) === true ? [{ id: 'off', enabled: false }] : []),
              ...[1024, 4096, 8192].map((budget_tokens) => ({
                id: `budget_${budget_tokens}`,
                label: `${budget_tokens.toLocaleString('en-US')} tokens`,
                budget_tokens,
              })),
            ],
          },
          protocol,
        )
      if (effort?.supported === false && thinking?.supported === false)
        return { source: 'metadata', adapter: 'none', options: [] }
      return undefined
    }
    if (provider === 'xai' && protocol !== 'anthropic_messages') {
      const values = levels(record(item.capabilities)?.reasoning_effort, true)
      return values
        ? values.length
          ? effortCapability(values, protocol)
          : { source: 'metadata', adapter: 'none', options: [] }
        : undefined
    }
    if (provider === 'openrouter' && protocol !== 'anthropic_messages') {
      const reasoning = record(item.reasoning)
      if (reasoning?.mandatory !== undefined && typeof reasoning.mandatory !== 'boolean')
        return undefined
      const values =
        reasoning?.supported_efforts === null
          ? ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']
          : levels(reasoning?.supported_efforts, true)
      const available = values?.filter((id) => !(reasoning?.mandatory === true && id === 'none'))
      return available
        ? available.length
          ? effortCapability(available, protocol)
          : { source: 'metadata', adapter: 'none', options: [] }
        : undefined
    }
    if (
      (provider === 'gemini' && item.thinking === false) ||
      (provider === 'kimi' && item.supports_reasoning === false)
    )
      return { source: 'metadata', adapter: 'none', options: [] }
    return undefined
  } catch {
    return undefined
  }
}
