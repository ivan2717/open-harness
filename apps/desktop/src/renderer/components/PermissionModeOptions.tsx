import { Check, Hand, ShieldAlert, ShieldCheck } from 'lucide-react'
import type { ApprovalMode } from '../../shared/contracts'
import { approvalModes } from '../lib/permissions'

export function PermissionModeIcon({ mode }: { mode?: ApprovalMode }) {
  const Icon = mode === 'full_access' ? ShieldAlert : mode === 'auto_review' ? ShieldCheck : Hand
  return <Icon size={15} />
}

export function PermissionModeOptions({
  mode,
  disabled,
  onSelect,
}: {
  mode?: ApprovalMode
  disabled?: boolean
  onSelect: (mode: ApprovalMode) => void
}) {
  return (
    <>
      <div className="composer-menu-heading">审批方式</div>
      {approvalModes.map((item) => (
        <button
          key={item.id}
          type="button"
          role="menuitemradio"
          aria-checked={mode === item.id}
          disabled={disabled}
          className={`composer-menu-item permission-mode-item${item.id === 'full_access' ? ' permission-full-access' : ''}`}
          onClick={() => onSelect(item.id)}
        >
          <PermissionModeIcon mode={item.id} />
          <span>
            <strong>{item.label}</strong>
            <small>{item.description}</small>
          </span>
          {mode === item.id && <Check size={15} className="menu-check" />}
        </button>
      ))}
      {mode === 'auto_review' && (
        <p className="permission-menu-hint">自动审查会产生少量模型用量。</p>
      )}
    </>
  )
}
