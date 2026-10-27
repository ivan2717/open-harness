import { Plus, X } from 'lucide-react'
import type { ConfigureModelInput } from '../../shared/contracts'
import {
  reasoningPresets,
  type ReasoningCapability,
  type ReasoningOption,
} from '../../shared/model-reasoning'
import { availableReasoningOptions, reasoningLabel } from '../lib/reasoning-settings'

export function ModelReasoningDefault({
  capability,
  outputLimit,
  value,
  onChange,
}: {
  capability: ReasoningCapability
  outputLimit: number
  value: string
  onChange: (value: string) => void
}) {
  const options = availableReasoningOptions(capability, outputLimit)
  return (
    <div className="model-reasoning-default">
      <label>
        思考设置
        <select value={value} onChange={(event) => onChange(event.target.value)}>
          <option value="">使用服务默认设置</option>
          {options.map((option) => (
            <option key={option.id} value={option.id}>
              {reasoningLabel(option.id, option)}
            </option>
          ))}
        </select>
      </label>
      <p className="model-limit-note">
        {capability.source === 'preset'
          ? '使用默认预设，可在高级设置中调整；具体选项以服务支持为准。'
          : capability.source === 'manual'
            ? '使用自定义思考配置。选择服务默认时不指定思考参数。'
            : capability.source === 'metadata'
              ? '已读取当前服务提供的思考选项。'
              : capability.source === 'unknown'
                ? '使用服务默认设置。需要时可在高级设置中调整。'
                : '使用已保存的思考配置。'}
      </p>
    </div>
  )
}

export function ModelReasoningCompatibility({
  protocol,
  value,
  resolved,
  onChange,
}: {
  protocol: ConfigureModelInput['protocol']
  value?: ReasoningCapability
  resolved?: ReasoningCapability
  onChange: (capability: ReasoningCapability | undefined) => void
}) {
  const presets = reasoningPresets(protocol)
  const preset = value
    ? presets.find((item) => item.capability.adapter === value.adapter)
    : undefined
  const budget = value?.adapter === 'anthropic_budget'
  const toggle = value?.adapter === 'chat_toggle' || value?.adapter === 'thinking_toggle'
  const canDisable = [
    'deepseek',
    'messages_effort',
    'anthropic_adaptive',
    'anthropic_budget',
  ].includes(value?.adapter ?? '')
  function updateOption(index: number, patch: Partial<ReasoningOption>) {
    if (!value) return
    onChange({
      ...value,
      options: value.options.map((option, position) =>
        position === index ? { ...option, ...patch } : option,
      ),
    })
  }
  return (
    <div className="model-reasoning-compatibility">
      <label>
        思考参数模板
        <select
          value={value ? (preset?.id ?? 'custom') : 'auto'}
          onChange={(event) => {
            const selected = presets.find((item) => item.id === event.target.value)
            onChange(selected ? structuredClone(selected.capability) : undefined)
          }}
          aria-describedby="reasoning-compatibility-help"
        >
          <option value="auto">自动（优先服务信息）</option>
          {presets.map((item) => (
            <option key={item.id} value={item.id}>
              {item.label}
            </option>
          ))}
          {value && !preset && <option value="custom">自定义</option>}
        </select>
      </label>
      <p id="reasoning-compatibility-help" className="model-limit-note">
        接口协议已在上方选择，这里只调整思考参数。自动设置优先采用服务信息；手动模板请按服务说明调整。
      </p>
      {!value && resolved && resolved.adapter !== 'none' && (
        <div className="model-reasoning-actions">
          <button
            type="button"
            onClick={() => onChange({ ...structuredClone(resolved), source: 'manual' })}
          >
            自定义当前选项
          </button>
        </div>
      )}
      {value && toggle && (
        <div className="model-reasoning-options">
          {value.options.map((option, index) => (
            <div className="model-reasoning-toggle-option" key={index}>
              <span>{reasoningLabel(option.id, option)}</span>
              <button
                type="button"
                aria-label={`移除${reasoningLabel(option.id, option)}选项`}
                onClick={() =>
                  onChange({
                    ...value,
                    options: value.options.filter((_, position) => position !== index),
                  })
                }
              >
                <X size={15} />
              </button>
            </div>
          ))}
        </div>
      )}
      {value && value.adapter !== 'none' && !toggle && (
        <>
          <div className="model-reasoning-options">
            {value.options.map((option, index) => (
              <div className="model-reasoning-option" key={index}>
                <label>
                  选项标识
                  <input
                    aria-label={`思考选项 ${index + 1} 标识`}
                    value={option.id}
                    required
                    pattern="[a-z][a-z0-9_]{0,31}"
                    maxLength={32}
                    placeholder={budget ? 'budget_4096' : 'high'}
                    onChange={(event) => updateOption(index, { id: event.target.value })}
                  />
                </label>
                {option.enabled === false ? (
                  <span className="model-reasoning-disabled">关闭思考</span>
                ) : (
                  <label>
                    {budget ? '预算（tokens）' : '发送值'}
                    <input
                      aria-label={`思考选项 ${index + 1} ${budget ? '预算' : '发送值'}`}
                      type={budget ? 'number' : 'text'}
                      required
                      {...(budget
                        ? { min: 1024, max: 8_000_000, step: 1 }
                        : { pattern: '[a-z][a-z0-9_]{0,31}', maxLength: 32 })}
                      value={budget ? (option.budget_tokens ?? '') : (option.effort ?? '')}
                      onChange={(event) =>
                        updateOption(
                          index,
                          budget
                            ? {
                                budget_tokens:
                                  event.target.value === ''
                                    ? undefined
                                    : Number(event.target.value),
                                label: undefined,
                              }
                            : { effort: event.target.value },
                        )
                      }
                    />
                  </label>
                )}
                <button
                  type="button"
                  aria-label={`移除思考选项 ${index + 1}`}
                  onClick={() =>
                    onChange({
                      ...value,
                      options: value.options.filter((_, position) => position !== index),
                    })
                  }
                >
                  <X size={15} />
                </button>
              </div>
            ))}
          </div>
          <div className="model-reasoning-actions">
            <button
              type="button"
              disabled={value.options.length >= 32}
              onClick={() =>
                onChange({
                  ...value,
                  options: [
                    ...value.options,
                    budget ? { id: '', budget_tokens: 4096 } : { id: '', effort: '' },
                  ],
                })
              }
            >
              <Plus size={14} />
              添加{budget ? '预算' : '档位'}
            </button>
            {canDisable && !value.options.some((option) => option.enabled === false) && (
              <button
                type="button"
                disabled={value.options.length >= 32}
                onClick={() =>
                  onChange({ ...value, options: [{ id: 'off', enabled: false }, ...value.options] })
                }
              >
                添加关闭选项
              </button>
            )}
          </div>
          <p className="model-limit-note">
            {budget
              ? '达到或超过单次输出上限的预算会暂时不可选。'
              : '选项标识用于选择档位；发送值填写服务商接受的值。例如 max 可映射为 xhigh。'}
          </p>
        </>
      )}
      {value && preset && value.adapter !== 'none' && (
        <div className="model-reasoning-actions">
          <button type="button" onClick={() => onChange(structuredClone(preset.capability))}>
            恢复此兼容模板
          </button>
        </div>
      )}
    </div>
  )
}
