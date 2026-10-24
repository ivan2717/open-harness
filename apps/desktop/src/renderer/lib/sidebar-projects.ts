import type { RunSummary } from '../../shared/contracts'

export interface SidebarProjectGroup {
  path: string
  name: string
  runs: RunSummary[]
  total: number
}

export function projectName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
}

function recentFirst(left: RunSummary, right: RunSummary): number {
  return (Date.parse(right.updated_at) || 0) - (Date.parse(left.updated_at) || 0)
}

export function sidebarConversations(runs: RunSummary[], projectPaths: string[], search: string) {
  const term = search.trim().toLocaleLowerCase()
  const matchesRun = (run: RunSummary) =>
    `${run.title} ${run.prompt}`.toLocaleLowerCase().includes(term)
  const groups = new Map<string, SidebarProjectGroup>()
  for (const path of projectPaths) {
    if (typeof path === 'string' && path && !groups.has(path))
      groups.set(path, { path, name: projectName(path), runs: [], total: 0 })
  }
  const independent: RunSummary[] = []
  for (const run of runs) {
    if (run.delegation) continue
    if (!run.workspace_path) {
      independent.push(run)
      continue
    }
    let group = groups.get(run.workspace_path)
    if (!group) {
      group = {
        path: run.workspace_path,
        name: projectName(run.workspace_path),
        runs: [],
        total: 0,
      }
      groups.set(run.workspace_path, group)
    }
    group.runs.push(run)
    group.total += 1
  }
  for (const group of groups.values()) group.runs.sort(recentFirst)
  const projects = [...groups.values()]
    .sort((left, right) => {
      const leftDate = Date.parse(left.runs[0]?.updated_at ?? '') || 0
      const rightDate = Date.parse(right.runs[0]?.updated_at ?? '') || 0
      return rightDate - leftDate
    })
    .map((group) => ({
      ...group,
      runs: group.path.toLocaleLowerCase().includes(term)
        ? group.runs
        : group.runs.filter(matchesRun),
    }))
    .filter((group) => group.runs.length || group.path.toLocaleLowerCase().includes(term))

  return {
    independent: independent.filter(matchesRun).sort(recentFirst),
    independentTotal: independent.length,
    projects,
  }
}
