import type { RunDetail, RunSummary } from '../../shared/contracts'

export function latestSnapshot(current: RunDetail | undefined, incoming: RunDetail): RunDetail {
  if (!current || current.id !== incoming.id) return recentSnapshot(incoming)
  return incoming.revision > current.revision ? recentSnapshot(incoming) : current
}

/** Mutation responses from older/API clients can still contain the full log. */
export function recentSnapshot(run: RunDetail): RunDetail {
  if (run.history || (run.messages.length <= 100 && run.events.length <= 300)) return run
  const messages = run.messages.slice(-100)
  const events = run.events.slice(-300)
  return {
    ...run,
    messages,
    events,
    history: {
      before: run.messages.length > messages.length ? messages[0].id : null,
      events_before: run.events.length > events.length ? events[0].sequence : null,
      total_messages: run.messages.length,
      total_events: run.events.length,
    },
  }
}

export function updateRunList(runs: RunSummary[] | undefined, run: RunSummary): RunSummary[] {
  const {
    id,
    revision,
    title,
    prompt,
    status,
    compacting,
    delegation,
    execution_policy,
    model_id,
    workspace_path,
    execution_directory,
    permission_mode,
    created_at,
    updated_at,
  } = run
  const summary: RunSummary = {
    id,
    revision,
    title,
    prompt,
    status,
    compacting,
    delegation,
    execution_policy,
    model_id,
    workspace_path,
    execution_directory,
    permission_mode,
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
