import { describe, expect, it } from 'vitest'
import type { ChatMessage, RunSummary } from '../../shared/contracts'
import { resolveAgentActivity } from './agent-activity'

function run(id: string, task_name?: string, root = 'root'): RunSummary {
  return {
    id,
    revision: 0,
    title: task_name ?? 'Main task',
    prompt: 'Task',
    status: 'completed',
    model_id: 'test',
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    ...(task_name ? { delegation: { task_name, root_run_id: root, parent_run_id: root } } : {}),
  }
}
const root = run('root')
const reader = run('child-reader', 'reader')
const reviewer = run('child-reviewer', 'reviewer')
const foreign = run('foreign-reader', 'reader', 'unrelated-root')
const runs = [root, reader, reviewer, foreign]

function message(
  tool_name: string,
  args: unknown,
  result: unknown,
  extra: Partial<ChatMessage> = {},
): ChatMessage {
  return {
    id: 'activity',
    role: 'tool',
    status: 'complete',
    tool_name,
    tool_arguments: args === undefined ? undefined : JSON.stringify(args),
    text: JSON.stringify(result),
    ...extra,
  }
}
const resolve = (item: ChatMessage, source = runs, parent = root.id) =>
  resolveAgentActivity(item, source, parent)
const members = runs.map((item) => ({
  agent_id: item.id,
  target: item.delegation ? `/root/${item.delegation.task_name}` : '/root',
  status: item.status,
}))

describe('agent activity navigation', () => {
  it('resolves the real spawn response using returned identity instead of display text', () => {
    expect(
      resolve(
        message(
          'spawn_agent',
          { task_name: 'reader', task: 'Read files' },
          {
            agent_id: reader.id,
            target: '/root/reader',
            status: 'running',
          },
        ),
      ),
    ).toEqual({ targets: [reader], label: 'reader' })
    expect(resolve(message('spawn_agent', undefined, { run_id: reviewer.id }))).toEqual({
      targets: [reviewer],
      label: 'reviewer',
    })
  })

  it('never turns a failed or pending spawn attempt into an existing child with the same name', () => {
    const item = message(
      'spawn_agent',
      { task_name: 'reader', task: 'Other task' },
      { error: 'duplicate name' },
    )
    expect(resolve(item)).toBeNull()
    expect(resolve({ ...item, is_error: true })).toBeNull()
    expect(resolve({ ...item, status: 'pending', text: item.tool_arguments! })).toBeNull()
  })

  it.each(['reader', '/root/reader', 'child-reader'])(
    'resolves targeted controls addressed by %s',
    (target) => {
      expect(
        resolve(
          message(
            'agent_control',
            { action: 'send', target, message: 'Check' },
            {
              message_id: 'message-one',
              target: '/root/reader',
              accepted: true,
              starts_idle_agent: false,
              active: false,
            },
          ),
        )?.targets,
      ).toEqual([reader])
      expect(
        resolve(
          message(
            'agent_control',
            { action: 'read', target },
            { messages: [], notice: 'Legacy preview' },
          ),
        )?.targets,
      ).toEqual([reader])
    },
  )

  it('uses returned IDs for result/read/stop and legacy records without saved arguments', () => {
    for (const action of ['result', 'read', 'interrupt', 'followup', 'resume', 'cancel']) {
      expect(
        resolve(message('agent_control', { action, agent_id: reader.id }, { agent_id: reader.id }))
          ?.targets,
      ).toEqual([reader])
    }
    expect(
      resolve(message('agent_control', undefined, { agent_id: reviewer.id, messages: [] }))
        ?.targets,
    ).toEqual([reviewer])
    // A failed child's result is still an inspectable child, not a failed tool call.
    expect(
      resolve(
        message(
          'agent_control',
          { action: 'result', target: 'reader' },
          {
            agent_id: reader.id,
            status: 'failed',
            error: 'Child command failed',
            reports: [],
          },
        ),
      )?.targets,
    ).toEqual([reader])
  })

  it('resolves targeted wait to only its target although the backend returns every member', () => {
    expect(
      resolve(
        message(
          'agent_control',
          { action: 'wait', target: 'reviewer' },
          {
            members,
            wait: { outcome: 'activity', progress_token: 'revision', waited_ms: 10 },
          },
        ),
      ),
    ).toEqual({ targets: [reviewer], label: 'reviewer' })
  })

  it('resolves an untargeted wait to unique existing children in this root only', () => {
    expect(
      resolve(
        message(
          'agent_control',
          { action: 'wait' },
          {
            members: [
              ...members,
              members[1],
              { agent_id: 'missing', target: '/root/reader' },
              null,
            ],
            wait: { outcome: 'timeout', progress_token: 'revision', waited_ms: 30000 },
          },
        ),
      ),
    ).toEqual({ targets: [reader, reviewer], label: '2 个协作任务' })
  })

  it('does not link list/models/unknown actions or non-collaboration messages', () => {
    for (const action of ['list', 'models', 'unknown', {}])
      expect(
        resolve(message('agent_control', { action }, { members, agent_id: reader.id })),
      ).toBeNull()
    expect(
      resolve(message('call_mcp_tool', { name: 'spawn_agent' }, { agent_id: reader.id })),
    ).toBeNull()
    expect(
      resolve(message('spawn_agent', {}, { agent_id: reader.id }, { role: 'assistant' })),
    ).toBeNull()
  })

  it('does not fall back to names for foreign, missing, conflicting, or malformed identities', () => {
    for (const result of [
      { agent_id: foreign.id, target: '/root/reader' },
      { agent_id: 'missing', target: '/root/reader' },
      { agent_id: null, target: '/root/reader' },
      { agent_id: reader.id, run_id: reviewer.id },
    ])
      expect(resolve(message('spawn_agent', { task_name: 'reader' }, result))).toBeNull()
    expect(
      resolve(
        message('agent_control', { action: 'read', target: 'reader' }, { agent_id: reviewer.id }),
      ),
    ).toBeNull()
    expect(
      resolve(
        message(
          'agent_control',
          { action: 'read', target: 'reader', agent_id: reader.id },
          { agent_id: reader.id },
        ),
      ),
    ).toBeNull()
    expect(
      resolve(message('agent_control', { action: 'read', target: foreign.id }, { messages: [] })),
    ).toBeNull()
    expect(
      resolve(message('agent_control', { action: 'read', target: 'reader' }, { messages: [] }), [
        ...runs,
        run('duplicate', 'reader'),
      ]),
    ).toBeNull()
  })

  it('ignores errors, invalid JSON, non-object arguments and unrelated nested evidence', () => {
    const item = message(
      'agent_control',
      { action: 'read', target: 'reader' },
      { agent_id: reader.id },
    )
    for (const extra of [
      { is_error: true },
      { status: 'interrupted' as const },
      { tool_arguments: 'bad-json' },
      { tool_arguments: '[]' },
      { tool_arguments: 'null' },
      { text: 'bad-json' },
      { text: '{"accepted":false}' },
    ])
      expect(resolve({ ...item, ...extra })).toBeNull()
    expect(
      resolve(
        message('agent_control', undefined, {
          report: { agent_id: reader.id },
          source: { run_id: reader.id },
        }),
      ),
    ).toBeNull()
  })

  it('uses known targets for pending controls but does not treat pending arguments as results', () => {
    expect(
      resolve(
        message(
          'agent_control',
          undefined,
          { action: 'read', target: 'reader' },
          { status: 'pending' },
        ),
      )?.targets,
    ).toEqual([reader])
    expect(
      resolve(message('agent_control', undefined, { agent_id: reader.id }, { status: 'pending' })),
    ).toBeNull()
  })

  it('opens projected inbox sender IDs within the same root and excludes self/root aliases', () => {
    expect(resolve(message(`协作 · ${reader.id}`, undefined, 'report'))?.targets).toEqual([reader])
    expect(resolve(message(`协作 · ${foreign.id}`, undefined, 'report'))).toBeNull()
    expect(
      resolve(
        message('agent_control', { action: 'read', target: 'reviewer' }, { agent_id: reviewer.id }),
        runs,
        reader.id,
      )?.targets,
    ).toEqual([reviewer])
    for (const target of ['self', 'parent', 'root', '/root', root.id, reader.id])
      expect(
        resolve(
          message('agent_control', { action: 'read', target }, { messages: [] }),
          runs,
          reader.id,
        ),
      ).toBeNull()
  })
})
