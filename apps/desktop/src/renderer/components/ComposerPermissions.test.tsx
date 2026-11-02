import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ApprovalMode } from '../../shared/contracts'
import { ComposerPermissions } from './ComposerPermissions'
import { PermissionModeOptions } from './PermissionModeOptions'
import { ProjectPicker } from './ProjectPicker'
import { useUiStore } from '../state/ui-store'

function render(runId?: string, mode: ApprovalMode = 'auto_review') {
  const client = new QueryClient()
  if (runId)
    client.setQueryData(['run-permissions', runId], { approval_mode: mode, editable: true })
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <ComposerPermissions runId={runId} />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}
describe('composer approval modes', () => {
  afterEach(() => useUiStore.setState({ approvalMode: 'ask' }))
  it('defaults to asking and never carries full access into another new task', () => {
    expect(render()).toContain('请求批准')
    useUiStore.getState().setApprovalMode('full_access')
    expect(useUiStore.getState().approvalMode).toBe('full_access')
    useUiStore.getState().resetFullAccessDraft()
    expect(useUiStore.getState().approvalMode).toBe('ask')
    useUiStore.getState().setApprovalMode('auto_review')
    useUiStore.getState().resetFullAccessDraft()
    expect(useUiStore.getState().approvalMode).toBe('auto_review')
  })
  it('uses the same compact menu for saved conversations without a management dialog', () => {
    const html = render('root')
    expect(html).toContain('帮我批准')
    expect(html).toContain('aria-haspopup="menu"')
    expect(html).not.toContain('aria-haspopup="dialog"')
    expect(html).not.toContain('disabled=')
    expect(render('root', 'full_access')).toContain('permission-full-access')
  })
  it('offers exactly three modes without file controls or grant management', () => {
    for (const mode of ['ask', 'auto_review', 'full_access'] as const) {
      const html = renderToStaticMarkup(<PermissionModeOptions mode={mode} onSelect={vi.fn()} />)
      expect(html.match(/role="menuitemradio"/g)).toHaveLength(3)
      expect(html.match(/aria-checked="true"/g)).toHaveLength(1)
      expect(html).toContain('请求批准')
      expect(html).toContain('帮我批准')
      expect(html).toContain('完全访问权限')
      expect(html).toContain('无需逐次批准，可联网并读写电脑上的文件。')
      expect(html.includes('模型用量')).toBe(mode === 'auto_review')
      expect(html).not.toContain('会话文件')
      expect(html).not.toContain('已批准的访问')
      expect(html).not.toContain('<select')
    }
  })
  it('does not make users configure a second project permission switch', () => {
    const html = renderToStaticMarkup(
      <ProjectPicker project={{ path: '/project', permission: 'read_only' }} onChange={vi.fn()} />,
    )
    expect(html).toContain('project')
    expect(html).not.toContain('只读')
    expect(html).not.toContain('项目权限')
    expect(html).not.toContain('允许修改')
  })
})
