import { useState, type FormEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronRight } from 'lucide-react'
import type { ConfigureModelInput } from '../../shared/contracts'
import { resolveModelLimits, type ModelCapacity } from '../../shared/model-limits'
import {
  resolveReasoningCapability,
  validateReasoningCapability,
  type ReasoningCapability,
} from '../../shared/model-reasoning'
import { configOptions } from '../lib/queries'
import { errorMessage } from '../lib/format'
import { availableReasoningOptions } from '../lib/reasoning-settings'
import { useUiStore } from '../state/ui-store'
import { InlineError } from './Feedback'
import { ModelReasoningCompatibility, ModelReasoningDefault } from './ModelReasoningSettings'

export function ModelSettings() {
  const client = useQueryClient()
  const select = useUiStore((state) => state.setSelectedModel)
  const [protocol, setProtocol] = useState<ConfigureModelInput['protocol']>('chat_completions')
  const [reasoningOverride, setReasoningOverride] = useState<ReasoningCapability>()
  const [discoveredReasoning, setDiscoveredReasoning] = useState<ReasoningCapability>()
  const [reasoningDefault, setReasoningDefault] = useState('')
  const [formError, setFormError] = useState<string>()
  const [key, setKey] = useState('')
  const [baseUrl, setBaseUrl] = useState('')
  const [modelName, setModelName] = useState('')
  const [contextOverride, setContextOverride] = useState('')
  const [outputOverride, setOutputOverride] = useState('')
  const [capacity, setCapacity] = useState<ModelCapacity>()
  const save = useMutation({
    mutationFn: (input: ConfigureModelInput) => window.harness.configureModel(input),
    onSuccess: async (result, input) => {
      setKey('')
      if (
        result.capacity ||
        input.context_window === undefined ||
        input.max_output_tokens === undefined
      )
        setCapacity(result.capacity)
      const discovered = result.model.reasoning?.capability
      setDiscoveredReasoning(
        discovered?.source === 'metadata' || discovered?.source === 'preset'
          ? discovered
          : undefined,
      )
      select(result.model.id)
      await client.invalidateQueries({ queryKey: configOptions.queryKey })
    },
  })
  const customWindow = Number(contextOverride)
  const limits = resolveModelLimits(
    {
      protocol,
      base_url: baseUrl,
      model: modelName,
      ...(contextOverride !== '' &&
      Number.isSafeInteger(customWindow) &&
      customWindow >= 1024 &&
      customWindow <= 8_000_000
        ? { context_window: customWindow }
        : {}),
    },
    capacity,
  )
  const customized = contextOverride !== '' || outputOverride !== ''
  const capability =
    reasoningOverride ??
    resolveReasoningCapability(
      { protocol, base_url: baseUrl, model: modelName },
      discoveredReasoning,
    )
  const outputLimit =
    outputOverride !== '' && Number(outputOverride) > 0
      ? Number(outputOverride)
      : limits.max_output_tokens
  const selectedReasoning = availableReasoningOptions(capability, outputLimit).some(
    (option) => option.id === reasoningDefault,
  )
    ? reasoningDefault
    : ''
  function resetLimits() {
    setContextOverride('')
    setOutputOverride('')
    save.reset()
    setFormError(undefined)
  }
  function resetModel() {
    setCapacity(undefined)
    setDiscoveredReasoning(undefined)
    setReasoningOverride(undefined)
    setReasoningDefault('')
    resetLimits()
  }
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    setFormError(undefined)
    try {
      if (
        reasoningOverride &&
        reasoningOverride.adapter !== 'none' &&
        !reasoningOverride.options.length
      )
        throw new Error('请至少保留一个思考选项，或选择“不提供思考控制”。')
      if (reasoningOverride) validateReasoningCapability(reasoningOverride, protocol)
    } catch (error) {
      setFormError(errorMessage(error))
      return
    }
    save.mutate({
      protocol,
      base_url: baseUrl.trim(),
      model: modelName.trim(),
      api_key: key,
      ...(contextOverride !== '' ? { context_window: Number(contextOverride) } : {}),
      ...(outputOverride !== '' ? { max_output_tokens: Number(outputOverride) } : {}),
      // Keep the legacy fields readable by older vaults. New settings use a
      // capability and an independent default, including a null service default.
      reasoning_mode: 'off',
      reasoning_effort: 'medium',
      ...(reasoningOverride ? { reasoning_capability: reasoningOverride } : {}),
      reasoning_default: selectedReasoning || null,
      supports_images: data.get('supports_images') === 'on',
    })
  }
  return (
    <section className="settings-section">
      <h2>接入模型</h2>
      <p>填写模型连接信息，容量与输出预算会自动设置。</p>
      <form className="model-form" onSubmit={submit}>
        <fieldset disabled={save.isPending}>
          <label>
            接口协议
            <select
              value={protocol}
              onChange={(event) => {
                setProtocol(event.target.value as ConfigureModelInput['protocol'])
                resetModel()
              }}
            >
              <option value="chat_completions">Chat Completions</option>
              <option value="responses">Responses</option>
              <option value="anthropic_messages">Messages</option>
            </select>
          </label>
          <label>
            API 基础地址
            <input
              name="base_url"
              type="url"
              required
              placeholder="https://api.example.com/v1"
              autoComplete="off"
              value={baseUrl}
              onChange={(event) => {
                setBaseUrl(event.target.value)
                resetModel()
              }}
            />
          </label>
          <label>
            模型名称
            <input
              name="model"
              required
              maxLength={256}
              placeholder="服务商提供的 model ID"
              autoComplete="off"
              value={modelName}
              onChange={(event) => {
                setModelName(event.target.value)
                resetModel()
              }}
            />
          </label>
          <label>
            API key
            <input
              name="api_key"
              type="password"
              required
              maxLength={4096}
              autoComplete="off"
              value={key}
              onChange={(event) => {
                setKey(event.target.value)
                setDiscoveredReasoning(undefined)
                setReasoningDefault('')
                save.reset()
              }}
            />
          </label>
          <label className="model-capability-option">
            <input
              type="checkbox"
              name="supports_images"
              aria-labelledby="model-image-support-label"
              aria-describedby="model-image-support-help"
            />
            <span className="model-capability-copy">
              <span id="model-image-support-label">此模型支持图片输入</span>
              <small id="model-image-support-help">
                开启后可查看图片和操作截图，请确认模型支持视觉输入。
              </small>
            </span>
          </label>
          <ModelReasoningDefault
            capability={capability}
            outputLimit={outputLimit}
            value={selectedReasoning}
            onChange={(value) => {
              setReasoningDefault(value)
              save.reset()
            }}
          />
          <details
            className="model-advanced"
            onInvalidCapture={(event) => {
              event.currentTarget.open = true
            }}
          >
            <summary>
              <ChevronRight size={14} className="model-advanced-chevron" aria-hidden="true" />
              高级设置
              <span className="model-advanced-state">
                {customized || reasoningOverride ? '已自定义' : '默认'}
              </span>
            </summary>
            <ModelReasoningCompatibility
              protocol={protocol}
              value={reasoningOverride}
              resolved={capability}
              onChange={(value) => {
                setReasoningOverride(value)
                setReasoningDefault('')
                setFormError(undefined)
                save.reset()
              }}
            />
            <p className="model-limit-note">
              {limits.context_source === 'metadata'
                ? '已采用服务商提供的容量。'
                : limits.context_source === 'catalog'
                  ? '已根据模型资料自动设置，无需填写。'
                  : limits.context_source === 'override'
                    ? '使用你填写的容量，输出预算仍可自动设置。'
                    : '保存时自动读取模型信息；未识别时默认使用 1M（1,000,000 tokens），不代表已确认模型容量。'}
              留空即可使用自动设置。
            </p>
            <div className="model-form-row">
              <label>
                上下文窗口（tokens）
                <input
                  name="context_window"
                  type="number"
                  min={1024}
                  max={8_000_000}
                  step={1}
                  placeholder={`自动（${limits.context_window}）`}
                  value={contextOverride}
                  onChange={(event) => {
                    setContextOverride(event.target.value)
                    save.reset()
                  }}
                />
              </label>
              <label>
                单次输出上限（tokens）
                <input
                  name="max_output_tokens"
                  type="number"
                  min={64}
                  max={8_000_000}
                  step={1}
                  placeholder={`自动（${limits.max_output_tokens}）`}
                  value={outputOverride}
                  onChange={(event) => {
                    setOutputOverride(event.target.value)
                    if (capability.adapter === 'anthropic_budget') setReasoningDefault('')
                    save.reset()
                  }}
                />
              </label>
            </div>
            <div className="model-limit-footer">
              <p className="model-limit-note">单次输出包含思考和回复；系统会为输入保留空间。</p>
              <button type="button" disabled={!customized} onClick={resetLimits}>
                恢复容量自动设置
              </button>
            </div>
          </details>
          <button className="model-save" type="submit">
            {save.isPending ? '正在保存…' : '保存并使用'}
          </button>
        </fieldset>
        <p className="settings-footnote">
          密钥由系统加密后保存在此设备上。系统加密不可用时，仅在本次应用会话中使用。保存时可能读取模型信息，不会发起生成或计数请求。
        </p>
        {save.isError && <InlineError message={errorMessage(save.error)} />}
        {formError && <InlineError message={formError} />}
        {save.isSuccess && (
          <p role="status" className="settings-footnote">
            {save.data.persisted
              ? '模型已接入，密钥已加密保存。'
              : '模型已接入，密钥未保存，重启后需重新填写。'}
          </p>
        )}
      </form>
    </section>
  )
}
