import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'

export const windowsScriptEnvironmentKey = ['windows-script-environment'] as const

function WindowsScriptEnvironmentSettings() {
  const client = useQueryClient()
  const status = useQuery({
    queryKey: windowsScriptEnvironmentKey,
    queryFn: () => window.harness.windowsScriptEnvironmentStatus(),
    retry: false,
  })
  const prepare = useMutation({
    mutationFn: () => window.harness.prepareWindowsScriptEnvironment(),
    onSuccess: (result) => client.setQueryData(windowsScriptEnvironmentKey, result),
  })
  return (
    <section className="settings-section">
      <h2>本地脚本</h2>
      <div className="settings-row">
        <div>
          <strong>Windows 脚本环境</strong>
          <p role="status">
            {prepare.isPending
              ? '等待 Windows 系统确认…'
              : status.isPending
                ? '正在检查…'
                : status.isError
                  ? '暂时无法检查'
                  : status.data.ready
                    ? '已就绪'
                    : '需要首次准备'}
          </p>
        </div>
        {status.data?.ready ? (
          <button
            className="button button-ghost"
            disabled={status.isFetching}
            onClick={() => void status.refetch()}
          >
            检查状态
          </button>
        ) : (
          <button
            className="button button-secondary"
            disabled={prepare.isPending || !status.data || status.isFetching}
            onClick={() => prepare.mutate()}
          >
            {prepare.isPending ? '正在准备…' : '准备脚本环境'}
          </button>
        )}
      </div>
      <p className="settings-footnote">
        首次准备需要 Windows
        管理员确认；只补齐运行库所需的目录信息读取权限，不会提升任务或脚本的权限。
      </p>
      {(status.error || prepare.error) && (
        <InlineError
          message={errorMessage(prepare.error ?? status.error)}
          retry={() => {
            prepare.reset()
            void status.refetch()
          }}
        />
      )}
    </section>
  )
}

export function WindowsScriptSettings() {
  return window.harness.platform === 'win32' ? <WindowsScriptEnvironmentSettings /> : null
}
