import type { RunDetail, RunSummary } from '../../shared/contracts'

export function latestSnapshot(current: RunDetail | undefined, incoming: RunDetail): RunDetail {
  if (!current || current.id !== incoming.id) return incoming
  return incoming.revision > current.revision ? incoming : current
}

export function updateRunList(runs: RunSummary[] | undefined, run: RunSummary): RunSummary[] {
  const { id, revision, title, prompt, status, model_id, created_at, updated_at } = run
  const summary: RunSummary = {
    id,
    revision,
    title,
    prompt,
    status,
    model_id,
    created_at,
    updated_at,
  }
  return [summary, ...(runs ?? []).filter((item) => item.id !== id)].sort((a, b) =>
    b.updated_at.localeCompare(a.updated_at),
  )
}

// A request started before a new run or stream event must not erase that update.
// There is no delete operation; all known runs remain part of this workspace.
export function mergeRunLists(
  current: RunSummary[] | undefined,
  incoming: RunSummary[],
): RunSummary[] {
  const byId = new Map((current ?? []).map((run) => [run.id, run]))
  for (const run of incoming) {
    const old = byId.get(run.id)
    if (!old || run.revision > old.revision) byId.set(run.id, run)
  }
  return [...byId.values()].sort((a, b) => b.updated_at.localeCompare(a.updated_at))
}
