import { Link } from '@tanstack/react-router'
import { Check, ChevronDown, Settings2 } from 'lucide-react'
import type { ModelInfo } from '../../shared/contracts'
import { ComposerMenu } from './ComposerMenu'

export function ComposerModel({
  models,
  modelId,
  onChange,
  disabled,
}: {
  models: ModelInfo[]
  modelId: string
  onChange?: (id: string) => void
  disabled?: boolean
}) {
  const model = models.find((item) => item.id === modelId)
  if (!onChange)
    return (
      <span
        className="composer-model-name"
        title={`${model?.name ?? modelId} · 此会话的模型已固定；更换模型请新建会话。`}
      >
        {model?.name ?? modelId}
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
      title={model?.name}
      trigger={
        <>
          <span className="truncate">{model?.name ?? '选择模型'}</span>
          <ChevronDown size={13} />
        </>
      }
    >
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
      <div role="separator" className="composer-menu-separator" />
      <Link to="/models" role="menuitem" className="composer-menu-item">
        <Settings2 size={15} />
        添加／管理模型
      </Link>
    </ComposerMenu>
  )
}
