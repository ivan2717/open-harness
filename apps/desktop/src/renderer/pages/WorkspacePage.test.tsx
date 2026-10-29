import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import type { ModelInfo, RunDetail, WorkspaceReview } from '../../shared/contracts'
import { RunPage } from './WorkspacePage'

vi.mock('@tanstack/react-router', () => ({
  Link: ({ to: _to, params, children, ...props }: any) => (
    <a href={`/runs/${params.runId}`} {...props}>
      {children}
    </a>
  ),
  useNavigate: () => vi.fn(),
}))
vi.mock('../components/Conversation', () => ({
  Conversation: ({ run }: { run: RunDetail }) => (
    <article>{run.messages.map((message) => message.text).join('\n')}</article>
  ),
}))
const composer = vi.hoisted(() => ({ props: {} as Record<string, any> }))
vi.mock('../components/Composer', () => ({
  Composer: (props: Record<string, any>) => {
    composer.props = props
    return <textarea aria-label="任务输入" />
  },
}))
vi.mock('../components/ExtensionApprovals', () => ({ ExtensionApprovals: () => null }))

function render(
  overrides: Partial<RunDetail> = {},
  models?: Partial<ModelInfo>[],
  workspace?: WorkspaceReview,
): string {
  const run: RunDetail = {
    id: 'task',
    revision: 1,
    title: '协作检查',
    prompt: '检查',
    status: 'completed',
    model_id: 'test',
    created_at: '2026-10-24T08:00:00Z',
    updated_at: '2026-10-24T08:00:00Z',
    messages: [{ id: 'result', role: 'assistant', text: '检查结果', status: 'complete' }],
    events: [],
    context: null,
    ...overrides,
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  client.setQueryData(['run', run.id], run)
  client.setQueryData(['runs'], [run])
  if (workspace) client.setQueryData(['workspace', run.id], workspace)
  client.setQueryData(['config'], {
    models: models ?? [{ id: 'test', name: 'Test', available: true }],
  })
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <RunPage runId={run.id} />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}

const delegation = {
  parent_run_id: 'parent',
  root_run_id: 'root',
  task_name: '检查',
  write_paths: [],
}

describe('delegated task and pause controls', () => {
  it('lets the user approve a read-only command while other commands are running', () => {
    const html = render({ status: 'running', workspace_path: '/project' }, undefined, {
      root: '/project',
      permission_mode: 'workspace_write',
      busy: true,
      sandbox_available: true,
      changes: [],
      commands: [],
      approvals: [
        {
          id: 'readonly',
          command: 'python inspect.py',
          cwd: '/project',
          access: 'read_only',
          execution_mode: 'sandboxed',
        },
      ],
    })
    expect(html).toMatch(/<button(?![^>]*disabled)[^>]*>仅允许本次<\/button>/)
    expect(html).toMatch(/<button(?![^>]*disabled)[^>]*>允许<\/button>/)
  })
  it('keeps child results visible but directs further instructions to its parent', () => {
    const html = render({ delegation, status: 'failed' })
    expect(html).toContain('检查结果')
    expect(html).toContain('href="/runs/parent"')
    expect(html).toContain('返回父任务')
    expect(html).not.toContain('<textarea')
    expect(html).not.toContain('继续任务')
  })

  it('lets the user stop a running child without displaying a composer', () => {
    const html = render({ delegation, status: 'running' })
    expect(html).toContain('停止任务')
    expect(html).not.toContain('<textarea')
  })

  it('presents a paused root as resumable progress rather than an error', () => {
    const html = render({
      status: 'paused',
      error: 'legacy budget error',
      events: [
        {
          sequence: 1,
          step: 10,
          type: 'run_paused',
          label: '暂停',
          detail: '执行预算已用完，进度已保存。',
          timestamp: '',
        },
      ],
    })
    expect(html).toContain('执行预算已用完，进度已保存。')
    expect(html).toContain('继续任务')
    expect(html).toContain('<textarea')
    expect(html).not.toContain('legacy budget error')
    expect(html).not.toContain('role="alert"')
  })
})

describe('conversation model selection', () => {
  it('keeps model switching available when an old configuration is missing', () => {
    render({ reasoning_effort: 'high' }, [
      { id: 'test', name: 'Unavailable', available: false },
      { id: 'replacement', name: 'Replacement', available: true },
      { id: 'local-demo', name: 'Demo', available: true },
    ])
    expect(composer.props.modelId).toBe('test')
    expect(composer.props.reasoningEffort).toBe('high')
    expect(composer.props.disabled).toBe(true)
    expect(composer.props.modelSettingsDisabled).toBeFalsy()
    expect(composer.props.onModelChange).toBeTypeOf('function')
    expect(composer.props.models.map((model: ModelInfo) => model.id)).toEqual([
      'test',
      'replacement',
    ])
  })

  it('blocks settings during compaction and never offers real providers in demo sessions', () => {
    render({ compacting: true })
    expect(composer.props.modelSettingsDisabled).toBe(true)
    expect(composer.props.modelSettingsDisabledReason).toContain('正在整理上下文')
    render({ model_id: 'local-demo' }, [
      { id: 'test', available: true },
      { id: 'local-demo', available: true },
    ])
    expect(composer.props.models.map((model: ModelInfo) => model.id)).toEqual(['local-demo'])
  })
})
