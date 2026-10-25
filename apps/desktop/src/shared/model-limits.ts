import type { ConfigureModelInput } from './contracts'

export interface ModelCapacity {
  context_window?: number
  max_output_tokens?: number
}

export type ModelLimitSource = 'metadata' | 'catalog' | 'fallback' | 'override'

export interface ResolvedModelLimits {
  context_window: number
  max_output_tokens: number
  context_source: ModelLimitSource
  output_source: ModelLimitSource
}

type ModelLimitInput = Pick<
  ConfigureModelInput,
  'protocol' | 'base_url' | 'model' | 'context_window' | 'max_output_tokens'
>

export const MODEL_TOKEN_LIMIT = 8_000_000
// Assumed capacity only when neither the caller nor the provider supplies a limit.
export const DEFAULT_MODEL_CONTEXT_WINDOW = 1_000_000

function tokenLimit(value: unknown, minimum: number): number | undefined {
  return typeof value === 'number' &&
    Number.isSafeInteger(value) &&
    value >= minimum &&
    value <= MODEL_TOKEN_LIMIT
    ? value
    : undefined
}

/** Only published, exact model IDs on the official API endpoint use this directory. */
function catalogCapacity(input: ModelLimitInput): ModelCapacity | undefined {
  let base: URL
  try {
    base = new URL(input.base_url.trim())
  } catch {
    return undefined
  }
  if (base.username || base.password || base.search || base.hash) return undefined
  const path = base.pathname.replace(/\/+$/, '')
  const model = input.model.trim()
  if (
    base.origin === 'https://api.openai.com' &&
    path === '/v1' &&
    input.protocol !== 'anthropic_messages' &&
    model === 'gpt-6-luna'
  ) {
    // https://developers.openai.com/api/docs/models/gpt-6-luna (checked 2026-10-22).
    return { context_window: 1_050_000, max_output_tokens: 128_000 }
  }
  if (
    base.origin === 'https://api.deepseek.com' &&
    (input.protocol === 'anthropic_messages'
      ? ['/anthropic', '/anthropic/v1'].includes(path)
      : ['', '/v1'].includes(path)) &&
    [
      'deepseek-flash',
      'deepseek-v4-pro',
      'deepseek-v4-flash',
      'deepseek-v4-flash-vision-exp',
    ].includes(model)
  ) {
    // https://api-docs.deepseek.com/api/list-models/ and /quick_start/pricing/ (2026-10-22).
    return { context_window: 1_048_576, max_output_tokens: 393_216 }
  }
  if (
    base.origin === 'https://api.anthropic.com' &&
    path === '/v1' &&
    input.protocol === 'anthropic_messages'
  ) {
    // https://platform.claude.com/docs/en/models/overview (checked 2026-10-22).
    if (['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-5-5'].includes(model))
      return { context_window: 1_000_000, max_output_tokens: 128_000 }
    // https://platform.claude.com/docs/en/models/haiku-4-5/overview (checked 2026-10-22).
    if (['claude-haiku-4-5', 'claude-haiku-4-5-20251001'].includes(model))
      return { context_window: 200_000, max_output_tokens: 64_000 }
  }
  return undefined
}

/** Shared offline preview; the main process may supply validated provider metadata on save. */
export function resolveModelLimits(
  input: ModelLimitInput,
  metadata?: ModelCapacity,
): ResolvedModelLimits {
  const catalog = catalogCapacity(input)
  const metadataContext = tokenLimit(metadata?.context_window, 1024)
  const metadataOutput = tokenLimit(metadata?.max_output_tokens, 64)
  if (input.context_window !== undefined && tokenLimit(input.context_window, 1024) === undefined)
    throw new Error('上下文窗口须为 1,024 至 8,000,000 的整数。')
  if (
    input.max_output_tokens !== undefined &&
    tokenLimit(input.max_output_tokens, 64) === undefined
  )
    throw new Error('单次输出上限须为 64 至 8,000,000 的整数。')
  const context_window =
    input.context_window ??
    metadataContext ??
    catalog?.context_window ??
    DEFAULT_MODEL_CONTEXT_WINDOW
  const max_output_tokens =
    input.max_output_tokens ??
    Math.min(
      8192,
      metadataOutput ?? catalog?.max_output_tokens ?? 8192,
      Math.floor(context_window / 4),
    )
  const safetyMargin = Math.min(2048, Math.max(64, Math.floor(context_window / 100)))
  if (max_output_tokens + safetyMargin >= context_window)
    throw new Error('单次输出上限过大，请给输入和安全余量保留空间。')
  return {
    context_window,
    max_output_tokens,
    context_source:
      input.context_window !== undefined
        ? 'override'
        : metadataContext !== undefined
          ? 'metadata'
          : catalog?.context_window !== undefined
            ? 'catalog'
            : 'fallback',
    output_source:
      input.max_output_tokens !== undefined
        ? 'override'
        : metadataOutput !== undefined
          ? 'metadata'
          : catalog?.max_output_tokens !== undefined
            ? 'catalog'
            : 'fallback',
  }
}
