import type { ReasoningCapability, ReasoningOption } from '../../shared/model-reasoning'

export function reasoningLabel(id: string, option?: ReasoningOption): string {
  if (option?.label) return option.label
  if (option?.enabled === false) return '关闭思考'
  if (option?.budget_tokens !== undefined) return `${option.budget_tokens.toLocaleString()} tokens`
  if (option?.enabled === true && option.effort === undefined) return '开启思考'
  const labels: Record<string, string> = {
    none: '关闭思考',
    off: '关闭思考',
    on: '开启思考',
    minimal: '最低',
    low: '低',
    medium: '中',
    high: '高',
    xhigh: '极高',
    max: '最高',
    ultra: 'Ultra',
  }
  return labels[id] ?? id
}

export function availableReasoningOptions(
  capability: ReasoningCapability,
  outputLimit: number,
): ReasoningOption[] {
  const ids = new Set<string>()
  return capability.options.filter((option) => {
    // Advanced settings may contain an unfinished row while it is being edited.
    // Submission validates the whole declaration; the picker only renders usable IDs.
    if (!/^[a-z][a-z0-9_]{0,31}$/.test(option.id) || ids.has(option.id)) return false
    ids.add(option.id)
    return option.budget_tokens === undefined || option.budget_tokens < outputLimit
  })
}

export function reasoningControlLabel(capability?: ReasoningCapability): string {
  if (capability?.adapter === 'anthropic_budget') return '思考预算'
  if (capability?.adapter === 'chat_toggle' || capability?.adapter === 'thinking_toggle')
    return '思考开关'
  return '思考强度'
}
