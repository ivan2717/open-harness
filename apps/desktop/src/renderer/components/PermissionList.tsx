import type { ControlPermission } from '../../shared/extensions'
import { permissionKey, permissionLabel } from '../lib/permissions'

export function PermissionList({
  permissions,
  busy,
  onRevoke,
}: {
  permissions: ControlPermission[]
  busy: boolean
  onRevoke: (permission: ControlPermission) => void
}) {
  return (
    <ul className="permission-list">
      {permissions.map((permission) => (
        <li key={permissionKey(permission)}>
          <div>
            <strong>{permissionLabel(permission)}</strong>
            <small>
              {permission.lifetime === 'always' ? '跨会话保留 · 始终允许' : '本会话及协作任务'}
            </small>
          </div>
          <button
            type="button"
            className="permission-text-button"
            disabled={busy}
            aria-label={`撤销 ${permissionLabel(permission)}`}
            onClick={() => onRevoke(permission)}
          >
            撤销
          </button>
        </li>
      ))}
    </ul>
  )
}
