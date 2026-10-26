import { Link } from '@tanstack/react-router'
import { Check, ChevronDown, Settings2 } from 'lucide-react'
import type { ModelInfo } from '../../shared/contracts'
import { reasoningControlLabel, reasoningLabel } from '../lib/reasoning-settings'
import { ComposerMenu } from './ComposerMenu'

export function ComposerModel({
  models,
  modelId,
  onChange,
  reasoningEffort,
  onReasoningChange,
  disabled,
  disabledReason,
}: {
  models: ModelInfo[]
  modelId: string
  onChange?: (id: string) => void
  reasoningEffort?: string | null
  onReasoningChange?: (effort: string | null) => void
  disabled?: boolean
  disabledReason?: string
}) {
  const model = models.find((item) => item.id === modelId)
  const efforts = model?.reasoning?.supported_efforts ?? []
  const retainedEffort =
    reasoningEffort != null && !efforts.includes(reasoningEffort) ? reasoningEffort : null
  const capability = model?.reasoning?.capability
  const effortLabel = (id: string) =>
    reasoningLabel(
      id,
      capability?.options.find((option) => option.id === id),
    )
  const controlLabel = reasoningControlLabel(capability)
  const effectiveEffort = reasoningEffort ?? model?.reasoning?.default_effort
  const defaultLabel = model?.reasoning?.default_effort
    ? `默认（${effortLabel(model.reasoning.default_effort)}）`
    : '服务默认'
  const selection = (
    <>
      <span className="truncate">{model?.name ?? (modelId || '选择模型')}</span>
      {effectiveEffort && (
        <span className="composer-reasoning-label">{effortLabel(effectiveEffort)}</span>
      )}
    </>
  )
  if (!onChange)
    return (
      <span className="composer-model-name" title={model?.name ?? modelId}>
        {selection}
      </span>
    )
  if (!models.length)
    return (
      <Link to="/models" className="composer-control add-model-link">
        添加模型
      </Link>
    )
  return (
    <ComposerMenu
      label="选择模型"
      disabled={disabled}
      align="end"
      className="composer-model-picker"
      title={disabledReason ?? '切换模型和思考强度'}
      trigger={
        <>
          {selection}
          <ChevronDown size={13} />
        </>
      }
    >
      <div role="group" aria-label="模型">
        <div className="composer-menu-heading">模型</div>
        {models.map((item) => (
          <button
            key={item.id}
            role="menuitemradio"
            aria-checked={item.id === modelId}
            disabled={!item.available}
            className="composer-menu-item"
            onClick={() => onChange(item.id)}
          >
            <span className="truncate">{item.name}</span>
            {item.id === modelId && <Check size={15} className="menu-check" />}
          </button>
        ))}
      </div>
      {onReasoningChange && (efforts.length > 0 || retainedEffort !== null) && (
        <>
          <div role="separator" className="composer-menu-separator" />
          <div role="group" aria-label={controlLabel}>
            <div className="composer-menu-heading">
              {controlLabel}
              {capability?.source === 'preset' && (
                <span
                  className="composer-reasoning-source"
                  title="这些选项来自默认预设，尚未获得当前模型的明确声明。"
                >
                  默认预设
                </span>
              )}
            </div>
            {[null, ...efforts].map((effort) => (
              <button
                key={effort === null ? 'inherited' : `option-${effort}`}
                role="menuitemradio"
                aria-checked={(reasoningEffort ?? null) === effort}
                className="composer-menu-item"
                disabled={!model?.available}
                onClick={() => onReasoningChange(effort)}
              >
                <span>{effort === null ? defaultLabel : effortLabel(effort)}</span>
                {(reasoningEffort ?? null) === effort && <Check size={15} className="menu-check" />}
              </button>
            ))}
            {retainedEffort !== null && (
              <button
                role="menuitemradio"
                aria-checked="true"
                disabled
                className="composer-menu-item"
              >
                <span>当前保留（{effortLabel(retainedEffort)}）</span>
                <Check size={15} className="menu-check" />
              </button>
            )}
          </div>
        </>
      )}
      <div role="separator" className="composer-menu-separator" />
      <Link to="/models" role="menuitem" className="composer-menu-item">
        <Settings2 size={15} />
        添加／管理模型
      </Link>
    </ComposerMenu>
  )
}
