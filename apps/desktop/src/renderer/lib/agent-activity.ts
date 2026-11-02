import type { ChatMessage, RunSummary } from '../../shared/contracts'

export interface AgentActivity {
  targets: RunSummary[]
  label: string
}

type RecordValue = Record<string, unknown>

function object(value: unknown): RecordValue | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as RecordValue)
    : null
}

function parse(value: string | undefined): RecordValue | null {
  try {
    return value ? object(JSON.parse(value)) : null
  } catch {
    return null
  }
}

const targetedActions = new Set([
  'send',
  'read',
  'result',
  'followup',
  'resume',
  'interrupt',
  'cancel',
])

/** Resolve recorded collaboration identities, never names found in report text. */
export function resolveAgentActivity(
  message: ChatMessage,
  runs: RunSummary[],
  parentRunId: string,
): AgentActivity | null {
  if (
    message.role !== 'tool' ||
    message.is_error ||
    !['pending', 'complete'].includes(message.status)
  )
    return null
  const parent = runs.find((run) => run.id === parentRunId)
  const rootId = parent?.delegation?.root_run_id ?? parentRunId
  const children = runs.filter(
    (run) => run.id !== parentRunId && run.id !== rootId && run.delegation?.root_run_id === rootId,
  )
  const byId = (id: unknown): RunSummary | undefined =>
    typeof id === 'string' ? children.find((run) => run.id === id) : undefined
  const byAddress = (address: unknown): RunSummary | undefined => {
    if (typeof address !== 'string' || ['self', 'root', '/root', 'parent'].includes(address))
      return undefined
    const exact = byId(address)
    if (exact) return exact
    const matches = children.filter(
      (run) =>
        run.delegation?.task_name === address || `/root/${run.delegation?.task_name}` === address,
    )
    return matches.length === 1 ? matches[0] : undefined
  }
  const finish = (targets: (RunSummary | undefined)[]): AgentActivity | null => {
    const unique = [...new Map(targets.filter((run) => !!run).map((run) => [run.id, run])).values()]
    if (!unique.length) return null
    return {
      targets: unique,
      label: unique.length === 1 ? unique[0].delegation!.task_name : `${unique.length} 个协作任务`,
    }
  }
  // Inbox projections carry the sender's actual run ID in the tool name.
  if (message.tool_name?.startsWith('协作 · '))
    return finish([byId(message.tool_name.slice('协作 · '.length))])
  if (message.tool_name !== 'spawn_agent' && message.tool_name !== 'agent_control') return null

  const argsText =
    message.tool_arguments ?? (message.status === 'pending' ? message.text : undefined)
  const args = parse(argsText)
  if (argsText !== undefined && !args) return null
  const result = message.status === 'complete' ? parse(message.text) : null
  if (message.status === 'complete' && (!result || result.accepted === false)) return null

  // Actual Rust responses use agent_id; accept run_id for persisted compatible records.
  // A declared but foreign/missing ID must never fall back to a coincidentally matching name.
  const resultHasId = !!result && ('agent_id' in result || 'run_id' in result)
  const resultTarget = resultHasId
    ? result!.run_id !== undefined &&
      result!.agent_id !== undefined &&
      result!.run_id !== result!.agent_id
      ? undefined
      : byId(result!.run_id ?? result!.agent_id)
    : byAddress(result?.target)
  if (resultHasId && !resultTarget) return null

  if (message.tool_name === 'spawn_agent') {
    // Pending/failed attempts with an already-used task name are not the existing child.
    return message.status === 'complete' && resultTarget?.delegation?.parent_run_id === parentRunId
      ? finish([resultTarget])
      : null
  }

  const action = args?.action
  if (message.status === 'pending' && action === undefined) return null
  if (action !== undefined && action !== 'wait' && !targetedActions.has(String(action))) return null
  if (args && 'target' in args && 'agent_id' in args) return null
  const hasAddress = !!args && ('target' in args || 'agent_id' in args)
  const requested = hasAddress ? byAddress(args!.target ?? args!.agent_id) : undefined
  if (hasAddress && (!requested || (resultTarget && resultTarget.id !== requested.id))) return null
  if (resultTarget) return finish([resultTarget])
  if (hasAddress) return finish([requested])
  if (action !== 'wait' || !Array.isArray(result?.members)) return null
  // Untargeted wait returns a member snapshot (including the root); preserve only known children.
  return finish(
    result.members.map((member) => {
      const entry = object(member)
      if (
        !entry ||
        (entry.run_id !== undefined &&
          entry.agent_id !== undefined &&
          entry.run_id !== entry.agent_id)
      )
        return undefined
      return byId(entry.run_id ?? entry.agent_id)
    }),
  )
}
