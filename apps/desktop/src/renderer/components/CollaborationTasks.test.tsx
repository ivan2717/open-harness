import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import type { RunSummary } from '../../shared/contracts'
import { CollaborationTasks } from './CollaborationTasks'

vi.mock('@tanstack/react-router', () => ({
  Link: ({ to: _to, params, children, ...props }: any) => (
    <a href={`/runs/${params.runId}`} {...props}>
      {children}
    </a>
  ),
}))

function render(runs: RunSummary[]): string {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  client.setQueryData(['runs'], runs)
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <CollaborationTasks rootId="root" />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}

function task(id: string, status: RunSummary['status'], rootId = 'root'): RunSummary {
  return {
    id,
    status,
    revision: 1,
    title: id,
    prompt: id,
    model_id: 'test',
    created_at: '2026-10-24T08:00:00Z',
    updated_at: '2026-10-24T08:00:00Z',
    delegation: { root_run_id: rootId, parent_run_id: rootId, task_name: id, write_paths: [] },
  }
}

describe('collaboration task list', () => {
  it('adds no UI when the conversation has no delegated tasks', () => {
    expect(render([task('unrelated', 'running', 'other')])).toBe('')
  })

  it('keeps results reachable, offers stop only while running, and starts collapsed', () => {
    const html = render([
      task('writer', 'running'),
      task('reviewer', 'completed'),
      task('later', 'paused'),
    ])
    expect(html).toContain('协作任务')
    expect(html).toContain('href="/runs/reviewer"')
    expect(html).toContain('href="/runs/later"')
    expect(html).toContain('停止协作任务：writer')
    expect(html).not.toContain('停止协作任务：reviewer')
    expect(html).not.toContain('停止协作任务：later')
    expect(html).toContain('aria-label="运行中"')
    expect(html).toContain('aria-label="空闲"')
    expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/)
  })
})
