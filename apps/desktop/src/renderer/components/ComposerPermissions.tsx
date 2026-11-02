import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown } from 'lucide-react'
import type { ApprovalMode } from '../../shared/contracts'
import { useUiStore } from '../state/ui-store'
import { approvalModes, refreshPermissions, runPermissionOptions } from '../lib/permissions'
import { errorMessage } from '../lib/format'
import { ComposerMenu } from './ComposerMenu'
import { PermissionModeIcon, PermissionModeOptions } from './PermissionModeOptions'
import '../permissions.css'

export function ComposerPermissions({ runId, disabled }: { runId?: string; disabled?: boolean }) {
  const client = useQueryClient()
  const draft = useUiStore((state) => state.approvalMode)
  const setDraft = useUiStore((state) => state.setApprovalMode)
  const query = useQuery({
    ...runPermissionOptions(runId ?? ''),
    enabled: Boolean(runId),
    refetchInterval: runId ? 5_000 : false,
    retry: 1,
  })
  const save = useMutation({
    mutationFn: ({ id, mode }: { id: string; mode: ApprovalMode }) =>
      window.harness.updateRunPermissions(id, {
        approval_mode: mode,
        permission_mode: 'workspace_write',
      }),
    onSuccess: (permissions, { id }) => {
      client.setQueryData(['run-permissions', id], permissions)
      refreshPermissions(client)
    },
    onError: () => refreshPermissions(client),
  })
  const mode = runId ? query.data?.approval_mode : draft
  const label = approvalModes.find((item) => item.id === mode)?.label ?? '审批方式'
  const busy = disabled || save.isPending || Boolean(runId && !query.data?.editable)
  const error = save.error ?? query.error
  return (
    <div className="composer-permissions-control">
      <ComposerMenu
        label="审批方式"
        className={`composer-permissions${mode === 'full_access' ? ' permission-full-access' : ''}`}
        trigger={
          <>
            <PermissionModeIcon mode={mode} />
            <span>{label}</span>
            <ChevronDown size={12} />
          </>
        }
        disabled={disabled || save.isPending || Boolean(runId && query.isPending)}
        title={
          runId && query.data && !query.data.editable
            ? '任务及协作任务空闲后可修改审批方式'
            : '选择任务的审批方式'
        }
      >
        <PermissionModeOptions
          mode={mode}
          disabled={busy}
          onSelect={(next) => {
            if (runId) {
              if (next !== mode || query.data?.permission_mode !== 'workspace_write')
                save.mutate({ id: runId, mode: next })
            } else setDraft(next)
          }}
        />
        {runId && query.data && !query.data.editable && (
          <p className="permission-menu-hint">任务及协作任务空闲后可修改。</p>
        )}
      </ComposerMenu>
      {error && (
        <button
          type="button"
          className="permission-mode-error"
          aria-label={`权限更新失败：${errorMessage(error)}。点击重试读取权限。`}
          title={errorMessage(error)}
          onClick={() => {
            save.reset()
            void query.refetch()
          }}
        >
          权限更新失败，重试
        </button>
      )}
    </div>
  )
}
