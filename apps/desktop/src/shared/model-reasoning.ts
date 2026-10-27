import type { ConfigureModelInput } from './contracts'

export type ReasoningSource = 'unknown' | 'catalog' | 'metadata' | 'manual' | 'legacy' | 'preset'
export type ReasoningAdapter =
  | 'none'
  | 'effort'
  | 'deepseek'
  | 'messages_effort'
  | 'messages_effort_only'
  | 'thinking_toggle'
  | 'anthropic_adaptive'
  | 'anthropic_budget'
  | 'chat_toggle'

export interface ReasoningOption {
  id: string
  label?: string
  effort?: string
  budget_tokens?: number
  enabled?: boolean
}

/** Declared controls, not evidence that a generation request has verified their effect. */
export interface ReasoningCapability {
  source: ReasoningSource
  adapter: ReasoningAdapter
  options: ReasoningOption[]
}

type Protocol = ConfigureModelInput['protocol']
type ReasoningInput = Pick<ConfigureModelInput, 'protocol' | 'base_url' | 'model'> &
  Partial<Pick<ConfigureModelInput, 'reasoning_capability' | 'reasoning_mode' | 'reasoning_effort'>>

export function validReasoningId(value: unknown): value is string {
  return typeof value === 'string' && value !== 'default' && /^[a-z][a-z0-9_]{0,31}$/.test(value)
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined
}

export function validateReasoningCapability(
  value: unknown,
  protocol: Protocol,
): ReasoningCapability {
  const input = record(value)
  const invalid = () => new Error('无效的模型思考能力配置')
  if (
    !input ||
    !['unknown', 'catalog', 'metadata', 'manual', 'legacy', 'preset'].includes(
      String(input.source),
    ) ||
    !Array.isArray(input.options) ||
    input.options.length > 32 ||
    Object.keys(input).some((key) => !['source', 'adapter', 'options'].includes(key))
  )
    throw invalid()
  const allowed: ReasoningAdapter[] =
    protocol === 'anthropic_messages'
      ? [
          'none',
          'messages_effort',
          'messages_effort_only',
          'anthropic_adaptive',
          'anthropic_budget',
        ]
      : protocol === 'responses'
        ? ['none', 'effort']
        : ['none', 'effort', 'deepseek', 'chat_toggle', 'thinking_toggle']
  if (!allowed.includes(input.adapter as ReasoningAdapter)) throw invalid()
  const adapter = input.adapter as ReasoningAdapter
  if (
    (adapter === 'none') !== (input.options.length === 0) ||
    (input.source === 'unknown' && adapter !== 'none')
  )
    throw invalid()
  const ids = new Set<string>()
  const options = input.options.map((value): ReasoningOption => {
    const option = record(value)
    if (
      !option ||
      !validReasoningId(option.id) ||
      ids.has(option.id) ||
      Object.keys(option).some(
        (key) => !['id', 'label', 'effort', 'budget_tokens', 'enabled'].includes(key),
      )
    )
      throw invalid()
    ids.add(option.id)
    if (
      option.label !== undefined &&
      (typeof option.label !== 'string' ||
        !option.label.trim() ||
        Array.from(option.label).length > 128 ||
        /\p{Cc}/u.test(option.label))
    )
      throw invalid()
    if (option.effort !== undefined && !validReasoningId(option.effort)) throw invalid()
    if (option.enabled !== undefined && typeof option.enabled !== 'boolean') throw invalid()
    const budget = option.budget_tokens
    if (
      budget !== undefined &&
      (typeof budget !== 'number' ||
        !Number.isSafeInteger(budget) ||
        budget < 1024 ||
        budget > 8_000_000)
    )
      throw invalid()
    if (adapter === 'effort' || adapter === 'messages_effort_only') {
      if (option.effort === undefined || option.enabled !== undefined || budget !== undefined)
        throw invalid()
    } else if (adapter === 'chat_toggle' || adapter === 'thinking_toggle') {
      if (option.enabled === undefined || option.effort !== undefined || budget !== undefined)
        throw invalid()
    } else if (adapter === 'anthropic_budget') {
      if (
        option.effort !== undefined ||
        (option.enabled === false ? budget !== undefined : budget === undefined)
      )
        throw invalid()
    } else if (
      option.enabled === false
        ? option.effort !== undefined || budget !== undefined
        : option.effort === undefined || budget !== undefined
    ) {
      throw invalid()
    }
    return {
      id: option.id,
      ...(option.label !== undefined ? { label: option.label as string } : {}),
      ...(option.effort !== undefined ? { effort: option.effort as string } : {}),
      ...(budget !== undefined ? { budget_tokens: budget as number } : {}),
      ...(option.enabled !== undefined ? { enabled: option.enabled as boolean } : {}),
    }
  })
  return { source: input.source as ReasoningSource, adapter, options }
}

export type ReasoningProvider =
  'openai' | 'deepseek' | 'anthropic' | 'xai' | 'gemini' | 'openrouter' | 'qwen' | 'kimi'

/** Identify the connection, never a model name advertised by an unrelated relay. */
export function officialReasoningProvider(
  input: Pick<ReasoningInput, 'base_url'>,
): ReasoningProvider | undefined {
  try {
    const url = new URL(input.base_url.trim())
    if (
      url.protocol !== 'https:' ||
      url.port ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      return undefined
    const host = url.hostname
    if (host === 'api.openai.com') return 'openai'
    if (host === 'api.deepseek.com') return 'deepseek'
    if (host === 'api.anthropic.com') return 'anthropic'
    if (host === 'api.x.ai') return 'xai'
    if (host === 'generativelanguage.googleapis.com') return 'gemini'
    if (host === 'openrouter.ai' || host === 'eu.openrouter.ai') return 'openrouter'
    if (host === 'api.moonshot.cn' || host === 'api.moonshot.ai') return 'kimi'
    if (
      [
        'dashscope.aliyuncs.com',
        'dashscope-intl.aliyuncs.com',
        'dashscope-us.aliyuncs.com',
        'cn-hongkong.dashscope.aliyuncs.com',
      ].includes(host) ||
      /^[a-z0-9-]+\.(cn-beijing|ap-northeast-1|eu-central-1|us-east-1)\.maas\.aliyuncs\.com$/.test(
        host,
      )
    )
      return 'qwen'
    return undefined
  } catch {
    return undefined
  }
}

/** An editable compatibility starting point, not a claim about this model's support. */
export function presetReasoningCapability(input: ReasoningInput): ReasoningCapability {
  const provider = officialReasoningProvider(input)
  const efforts = (levels: string[]) => levels.map((id) => ({ id, effort: id }))
  const preset = (adapter: ReasoningAdapter, options: ReasoningOption[]): ReasoningCapability => ({
    source: 'preset',
    adapter,
    options,
  })
  if (provider === 'deepseek') {
    const levels = efforts(['low', 'high', 'max'])
    return input.protocol === 'responses'
      ? preset('effort', [{ id: 'none', effort: 'none' }, ...levels])
      : preset(input.protocol === 'anthropic_messages' ? 'messages_effort' : 'deepseek', [
          { id: 'off', enabled: false },
          ...levels,
        ])
  }
  if (input.protocol === 'anthropic_messages') {
    if (provider === 'anthropic')
      return preset('messages_effort_only', efforts(['low', 'medium', 'high', 'xhigh', 'max']))
    if (provider === 'kimi') return preset('messages_effort_only', efforts(['low', 'high', 'max']))
    return preset('anthropic_budget', [
      { id: 'off', enabled: false },
      ...[1024, 4096, 8192].map((budget_tokens) => ({
        id: `budget_${budget_tokens}`,
        label: `${budget_tokens.toLocaleString('en-US')} tokens`,
        budget_tokens,
      })),
    ])
  }
  if (provider === 'qwen' && input.protocol === 'chat_completions')
    return preset('chat_toggle', [
      { id: 'off', enabled: false },
      { id: 'on', enabled: true },
    ])
  if (provider === 'kimi') return preset('effort', efforts(['low', 'high', 'max']))
  if (provider === 'xai') return preset('effort', efforts(['low', 'medium', 'high', 'xhigh']))
  if (provider === 'openai')
    return preset('effort', efforts(['low', 'medium', 'high', 'xhigh', 'max']))
  return preset('effort', efforts(['low', 'medium', 'high']))
}

/** These are opt-in compatibility templates, never automatic claims about a relay. */
export function reasoningPresets(
  protocol: Protocol,
): Array<{ id: string; label: string; capability: ReasoningCapability }> {
  const efforts = (levels: string[]) => levels.map((id) => ({ id, effort: id }))
  const off: ReasoningOption = { id: 'off', enabled: false }
  const preset = (id: ReasoningAdapter, label: string, options: ReasoningOption[]) => ({
    id,
    label,
    capability: { source: 'manual' as const, adapter: id, options },
  })
  const none = preset('none', '不提供思考控制', [])
  if (protocol === 'anthropic_messages')
    return [
      preset(
        'messages_effort_only',
        '仅思考强度',
        efforts(['low', 'medium', 'high', 'xhigh', 'max']),
      ),
      preset(
        'anthropic_adaptive',
        '自适应思考 + 强度',
        efforts(['low', 'medium', 'high', 'xhigh', 'max']),
      ),
      preset('anthropic_budget', '思考 token 预算', [
        off,
        ...[1024, 4096, 8192].map((tokens) => ({
          id: `budget_${tokens}`,
          label: `${tokens.toLocaleString('en-US')} tokens`,
          budget_tokens: tokens,
        })),
      ]),
      preset('messages_effort', '思考开关 + 强度', [off, ...efforts(['low', 'high', 'max'])]),
      none,
    ]
  if (protocol === 'responses')
    return [
      preset('effort', '思考强度', efforts(['none', 'low', 'medium', 'high', 'xhigh', 'max'])),
      none,
    ]
  return [
    preset('effort', '仅思考强度', efforts(['low', 'medium', 'high'])),
    preset('deepseek', '思考开关 + 强度', [off, ...efforts(['low', 'high', 'max'])]),
    preset('chat_toggle', '思考开关（enable_thinking）', [off, { id: 'on', enabled: true }]),
    preset('thinking_toggle', '思考开关（thinking.type）', [off, { id: 'on', enabled: true }]),
    none,
  ]
}

/** Preserve explicit legacy wire settings when no newer capability has been declared. */
function legacyCapability(input: ReasoningInput): ReasoningCapability | undefined {
  if (!input.reasoning_mode || input.reasoning_mode === 'off') return undefined
  const adapter: ReasoningAdapter =
    input.reasoning_mode === 'deepseek'
      ? 'deepseek'
      : input.protocol === 'anthropic_messages'
        ? 'anthropic_budget'
        : 'effort'
  return {
    source: 'legacy',
    adapter,
    options: ['low', 'medium', 'high'].map((id, index) =>
      adapter === 'anthropic_budget'
        ? { id, budget_tokens: [1024, 4096, 8192][index] }
        : { id, effort: id },
    ),
  }
}

export function resolveReasoningCapability(
  input: ReasoningInput,
  metadata?: ReasoningCapability,
): ReasoningCapability {
  if (
    input.reasoning_capability?.source === 'manual' ||
    input.reasoning_capability?.source === 'legacy'
  )
    return validateReasoningCapability(input.reasoning_capability, input.protocol)
  if (metadata)
    return validateReasoningCapability(
      { ...metadata, source: metadata.source === 'preset' ? 'preset' : 'metadata' },
      input.protocol,
    )
  return (
    (!input.reasoning_capability ? legacyCapability(input) : undefined) ??
    presetReasoningCapability(input)
  )
}
