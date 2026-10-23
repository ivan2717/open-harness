import { describe, expect, it } from 'vitest'
import type { RunSummary } from '../../shared/contracts'
import { sidebarConversations } from './sidebar-projects'

function run(id: string, workspace_path?: string, overrides: Partial<RunSummary> = {}): RunSummary {
  return {
    id,
    revision: 1,
    title: id,
    prompt: id,
    status: 'completed',
    model_id: 'local-demo',
    created_at: '2026-10-22T08:00:00Z',
    updated_at: '2026-10-22T08:00:00Z',
    workspace_path,
    ...overrides,
  }
}

describe('sidebar project organization', () => {
  it('keeps project conversations out of the independent list and preserves empty folders', () => {
    const result = sidebarConversations(
      [run('general'), run('in-a', '/projects/a'), run('in-b', '/projects/b')],
      ['/projects/a', '/projects/empty', '/projects/a'],
      '',
    )
    expect(result.independent.map((item) => item.id)).toEqual(['general'])
    expect(result.projects.map((item) => [item.path, item.runs.map((chat) => chat.id)])).toEqual([
      ['/projects/a', ['in-a']],
      ['/projects/b', ['in-b']],
      ['/projects/empty', []],
    ])
  })

  it('identifies projects by full path instead of merging folders with the same name', () => {
    const result = sidebarConversations(
      [run('left', '/work/client'), run('right', '/personal/client')],
      [],
      '',
    )
    expect(result.projects.map((item) => item.name)).toEqual(['client', 'client'])
    expect(result.projects.map((item) => item.path)).toEqual(['/work/client', '/personal/client'])
  })

  it('finds matching conversation text within its project and retains the total conversation count', () => {
    const result = sidebarConversations(
      [
        run('ordinary'),
        run('match', '/projects/one', { prompt: 'Investigate renderer navigation' }),
        run('other', '/projects/one'),
        run('different', '/projects/two'),
      ],
      ['/projects/empty'],
      '  NAVIGATION ',
    )
    expect(result.independent).toEqual([])
    expect(result.projects).toHaveLength(1)
    expect(result.projects[0].runs.map((item) => item.id)).toEqual(['match'])
    expect(result.projects[0].total).toBe(2)
  })

  it('includes all conversations when the project name or path matches a search', () => {
    const result = sidebarConversations(
      [run('one', 'C:\\work\\Client'), run('two', 'C:\\work\\Client'), run('other')],
      ['C:\\work\\Client', 'C:\\work\\Client docs'],
      'client',
    )
    expect(result.independent).toEqual([])
    expect(result.projects.map((item) => item.name)).toEqual(['Client', 'Client docs'])
    expect(result.projects[0].runs).toHaveLength(2)
    expect(result.projects[1].runs).toEqual([])
  })

  it('sorts groups and conversations by recent activity without reordering source data', () => {
    const runs = [
      run('old', '/work/a'),
      run('middle', '/work/b', { updated_at: '2026-10-22T09:00:00Z' }),
      run('new', '/work/a', { updated_at: '2026-10-22T10:00:00Z' }),
    ]
    const result = sidebarConversations(runs, [], '')
    expect(result.projects.map((item) => item.path)).toEqual(['/work/a', '/work/b'])
    expect(result.projects[0].runs.map((item) => item.id)).toEqual(['new', 'old'])
    expect(runs.map((item) => item.id)).toEqual(['old', 'middle', 'new'])
  })
})
