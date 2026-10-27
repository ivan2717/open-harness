import { renderToStaticMarkup } from 'react-dom/server'
import { Children, isValidElement, type ReactNode } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { CommandRecord, WorkspaceReview } from '../../shared/contracts'
import { CommandApprovals, RunningCommands, WorkspaceReviewPanel } from './WorkspaceReview'

const command: CommandRecord = {
  id: 'command-1',
  command: 'python report.py',
  status: 'running',
  output: 'Processing 10 rows…',
  exit_code: null,
}

describe('script command controls', () => {
  it('makes the platform execution boundary explicit before approval', () => {
    const approval = { id: 'command-1', command: command.command, cwd: '/conversation' }
    const unrestricted = renderToStaticMarkup(
      <CommandApprovals
        approvals={[{ ...approval, execution_mode: 'unrestricted' }]}
        busy={false}
        onDecide={vi.fn()}
      />,
    )
    expect(unrestricted).toContain('以你的系统账户权限运行')
    expect(unrestricted).toContain('可访问工作目录以外的文件和网络')
    expect(unrestricted).not.toContain('网络关闭')
    const sandboxed = renderToStaticMarkup(
      <CommandApprovals
        approvals={[{ ...approval, execution_mode: 'sandboxed' }]}
        busy={false}
        onDecide={vi.fn()}
      />,
    )
    expect(sandboxed).toContain('网络关闭')
    expect(sandboxed).not.toContain('可访问工作目录以外')
  })

  it.each([undefined, 'denied'] as const)(
    'keeps network off for a command with %s network access',
    (network_access) => {
      const html = renderToStaticMarkup(
        <CommandApprovals
          approvals={[
            {
              id: 'command-1',
              command: 'python report.py',
              cwd: '/conversation',
              execution_mode: 'sandboxed',
              network_access,
            },
          ]}
          busy={false}
          onDecide={vi.fn()}
        />,
      )
      expect(html).toContain('网络关闭')
      expect(html).toContain('允许执行这条命令？')
      expect(html).not.toContain('允许本次联网执行')
    },
  )

  it('explains network access and reusable scope without suggesting broader file access', () => {
    const html = renderToStaticMarkup(
      <CommandApprovals
        approvals={[
          {
            id: 'network-command',
            command: 'pnpm install',
            cwd: '/project',
            execution_mode: 'sandboxed',
            network_access: 'allowed',
          },
        ]}
        busy={false}
        onDecide={vi.fn()}
      />,
    )
    expect(html).toContain('允许执行并联网？')
    expect(html).toContain('允许命令及其子进程访问网络')
    expect(html).toContain('可读写工作目录及临时目录')
    expect(html).toContain('本会话授权适用于主任务及子任务')
    expect(html).toContain('此范围内的后续命令不再询问')
    expect(html).toContain('本会话同范围允许')
    expect(html).toContain('允许本次联网执行')
    expect(html).not.toContain('网络关闭')
    expect(html).not.toContain('可访问工作目录以外')
  })

  it('keeps one-time and session approval decisions distinct and describes read-only access', () => {
    const onDecide = vi.fn()
    const element = CommandApprovals({
      approvals: [
        {
          id: 'read',
          command: 'python inspect.py',
          cwd: '/project',
          execution_mode: 'sandboxed',
          access: 'read_only',
        },
      ],
      busy: false,
      onDecide,
    })
    const html = renderToStaticMarkup(element)
    expect(html).toContain('项目只读，文件写入仅限临时目录')
    expect(html).toContain('重启应用后失效')
    const buttons = collectButtons(element)
    buttons.find((button) => button.children === '允许本次')!.onClick()
    expect(onDecide).toHaveBeenLastCalledWith('read', true, 'once')
    buttons.find((button) => button.children === '本会话同范围允许')!.onClick()
    expect(onDecide).toHaveBeenLastCalledWith('read', true, 'session')
    buttons.find((button) => button.children === '拒绝')!.onClick()
    expect(onDecide).toHaveBeenLastCalledWith('read', false)
  })

  it('does not offer reusable permission outside an available sandbox', () => {
    for (const execution_mode of ['unrestricted', 'unavailable', undefined] as const) {
      const html = renderToStaticMarkup(
        <CommandApprovals
          approvals={[
            { id: 'legacy', command: 'python script.py', cwd: '/project', execution_mode },
          ]}
          busy={false}
          onDecide={vi.fn()}
        />,
      )
      expect(html).not.toContain('本会话同范围允许')
    }
  })

  it('cannot approve a stale request when command isolation is unavailable', () => {
    const html = renderToStaticMarkup(
      <CommandApprovals
        approvals={[
          {
            id: 'unavailable-command',
            command: command.command,
            cwd: '/conversation',
            execution_mode: 'unavailable',
          },
        ]}
        busy={false}
        onDecide={vi.fn()}
      />,
    )
    expect(html).toContain('命令隔离不可用，因此不会执行命令')
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>允许本次<\/button>/)
  })

  it('keeps background command inspection and stopping visible even without an active model turn', () => {
    const html = renderToStaticMarkup(
      <RunningCommands
        commands={[command, { ...command, id: 'done', command: 'old task', status: 'completed' }]}
        onInspect={vi.fn()}
        onStop={vi.fn()}
      />,
    )
    expect(html).toContain('python report.py')
    expect(html).toContain('查看输出')
    expect(html).toContain('停止命令：python report.py')
    expect(html).not.toContain('old task')
    const idle = renderToStaticMarkup(
      <RunningCommands commands={[]} onInspect={vi.fn()} onStop={vi.fn()} />,
    )
    expect(idle).toBe('')
  })

  it('shows live output and a stop action in the command tab for a managed workspace', () => {
    const review: WorkspaceReview = {
      root: '/conversation',
      permission_mode: 'workspace_write',
      busy: true,
      sandbox_available: false,
      execution_mode: 'unrestricted',
      changes: [],
      approvals: [],
      commands: [command],
    }
    const html = renderToStaticMarkup(
      <WorkspaceReviewPanel
        review={review}
        loading={false}
        busy={false}
        managed
        initialTab="commands"
        onClose={vi.fn()}
        onRefresh={vi.fn()}
        onRevert={vi.fn()}
        onStopCommand={vi.fn()}
        onRevealWorkspace={vi.fn()}
      />,
    )
    expect(html).toContain('会话文件与命令')
    expect(html).toContain('Processing 10 rows…')
    expect(html).toContain('停止命令')
    expect(html).toContain('不限制文件或网络访问')
    expect(html).not.toContain('因此不会执行命令')
  })

  it('retains requested network access without claiming denied requests were approved', () => {
    const review: WorkspaceReview = {
      root: '/conversation',
      permission_mode: 'workspace_write',
      busy: false,
      sandbox_available: true,
      execution_mode: 'sandboxed',
      changes: [],
      approvals: [],
      commands: [
        { ...command, status: 'completed', network_access: 'allowed' },
        { ...command, id: 'offline', status: 'completed', network_access: 'denied' },
        { ...command, id: 'legacy', status: 'completed' },
        { ...command, id: 'rejected', status: 'denied', network_access: 'allowed' },
      ],
    }
    const html = renderToStaticMarkup(
      <WorkspaceReviewPanel
        review={review}
        loading={false}
        busy={false}
        initialTab="commands"
        onClose={vi.fn()}
        onRefresh={vi.fn()}
        onRevert={vi.fn()}
        onStopCommand={vi.fn()}
        onRevealWorkspace={vi.fn()}
      />,
    )
    expect(html.match(/请求联网/g)).toHaveLength(2)
    expect(html).not.toContain('允许联网')
    expect(html).not.toContain('不限制文件或网络访问')
    expect(html).toContain('可修改项目')
  })

  it('shows revocable session grants even while a command is running', () => {
    const html = renderToStaticMarkup(
      <WorkspaceReviewPanel
        review={{
          root: '/project',
          permission_mode: 'workspace_write',
          busy: true,
          sandbox_available: true,
          changes: [],
          approvals: [],
          commands: [command],
          command_grants: [
            { access: 'read_only', network_access: 'denied' },
            { access: 'workspace_write', network_access: 'allowed' },
          ],
        }}
        loading={false}
        busy
        initialTab="commands"
        onClose={vi.fn()}
        onRefresh={vi.fn()}
        onRevert={vi.fn()}
        onStopCommand={vi.fn()}
        onRevealWorkspace={vi.fn()}
        onRevokeGrants={vi.fn()}
      />,
    )
    expect(html).toContain('项目只读 · 网络关闭')
    expect(html).toContain('可修改项目 · 允许联网')
    expect(html).toContain('重启应用后失效')
    expect(html).toContain('不会停止已经批准的命令')
    expect(html).toMatch(/<button(?![^>]*disabled)[^>]*>撤销会话授权<\/button>/)
  })

  it('identifies child-task requests and running commands without changing their command IDs', () => {
    const child = { ...command, id: 'child-command', task_title: '检查图表' }
    const onDecide = vi.fn()
    const approval = CommandApprovals({
      approvals: [{ ...child, cwd: '/project', execution_mode: 'sandboxed' }],
      busy: false,
      onDecide,
    })
    expect(renderToStaticMarkup(approval)).toContain('子任务：检查图表')
    collectButtons(approval)
      .find((button) => button.children === '允许本次')!
      .onClick()
    expect(onDecide).toHaveBeenCalledWith('child-command', true, 'once')
    const running = renderToStaticMarkup(
      <RunningCommands commands={[child]} onInspect={vi.fn()} onStop={vi.fn()} />,
    )
    expect(running).toContain('子任务：检查图表')
    const history = renderToStaticMarkup(
      <WorkspaceReviewPanel
        review={{
          root: '/project',
          permission_mode: 'workspace_write',
          busy: false,
          sandbox_available: true,
          changes: [],
          approvals: [],
          commands: [child],
        }}
        loading={false}
        busy={false}
        initialTab="commands"
        onClose={vi.fn()}
        onRefresh={vi.fn()}
        onRevert={vi.fn()}
        onStopCommand={vi.fn()}
        onRevealWorkspace={vi.fn()}
      />,
    )
    expect(history).toContain('子任务：检查图表')
  })
})

function collectButtons(node: ReactNode): { children?: ReactNode; onClick: () => void }[] {
  return Children.toArray(node).flatMap((child) => {
    if (!isValidElement<{ children?: ReactNode; onClick: () => void }>(child)) return []
    return child.type === 'button' ? [child.props] : collectButtons(child.props.children)
  })
}
