import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { childReadCorrection, correctionCited, childExecutedScript } from './continuity-evidence.mjs'

const correction = 'CORRECTION-ORCHID：R303 已核实为 settled，来源保持不变。'
const tool = (tool_name, output) => ({ role: 'tool', status: 'complete', is_error: false, tool_name, text: JSON.stringify(output) })
const history = (agent_id = 'root', id = 'event-4') => tool('agent_control', { agent_id,
  messages: [{ id, role: 'user', text: correction, truncated: false }] })

test('parent alias or raw ID is judged by the same returned source, not request spelling', () => {
  assert(childReadCorrection([{ id: 'child', messages: [history()] }], 'root', 4, correction))
  assert(childReadCorrection([{ id: 'child', messages: [tool('agent_control', {
    agent_id: 'root', message_id: 'event-4', role: 'user', text: correction, next_text_offset: null,
  })] }], 'root', 4, correction))
})

test('history read must return the real parent correction in full', () => {
  for (const message of [history('unrelated'), history('root', 'event-40'),
    { ...history(), is_error: true }, tool('agent_control', { agent_id: 'root',
      messages: [{ id: 'event-4', role: 'user', text: correction, truncated: true }] })]) {
    assert.equal(childReadCorrection([{ messages: [message] }], 'root', 4, correction), false)
  }
})

test('citation must identify the correction, not an arbitrary earlier event', () => {
  assert(correctionCited(['R303 settled; event-4'], 4))
  assert(correctionCited(['R303 已结算，CORRECTION-ORCHID'], 4))
  for (const citation of ['R303 settled; event-40', 'R303 settled; event-4abc', 'R303 settled; event-1', 'R303 pending; event-4']) {
    assert.equal(correctionCited([citation], 4), false)
  }
})

async function execution(t) {
  const workspace = await mkdtemp(join(tmpdir(), 'openharness-continuity-evidence-'))
  t.after(() => rm(workspace, { recursive: true, force: true }))
  await mkdir(join(workspace, '.openharness-tmp'))
  await writeFile(join(workspace, '.openharness-tmp/reconcile.py'), 'print(415)\n')
  const children = [{ id: 'child', messages: [tool('manage_command', {
    command_id: 'command-1', status: 'completed', exit_code: 0, output: '415\n',
  })] }]
  const reviews = [{ run_id: 'child', review: { review_incomplete: false, commands: [{
    id: 'command-1', command: '"/runtime with spaces/python3" -I -B .openharness-tmp/reconcile.py',
    status: 'completed', exit_code: 0, output: '415\n',
  }] } }]
  return { workspace, children, reviews }
}

test('binds successful child execution to host record, command ID and real script', async t => {
  const { workspace, children, reviews } = await execution(t)
  const result = await childExecutedScript(children, reviews, workspace)
  assert.equal(result.passed, true)
  assert.equal(result.command_id, 'command-1')
  assert.equal(result.script, '.openharness-tmp/reconcile.py')
})

test('unrelated success, parent execution, pending command and missing script do not pass', async t => {
  const { workspace, children, reviews } = await execution(t)
  const cases = []
  let changed = structuredClone(reviews); changed[0].review.commands[0].id = 'unrelated'; cases.push(changed)
  changed = structuredClone(reviews); changed[0].run_id = 'root'; cases.push(changed)
  changed = structuredClone(reviews); changed[0].review.commands[0].status = 'running'; cases.push(changed)
  changed = structuredClone(reviews); changed[0].review.commands[0].command = 'python3 .openharness-tmp/missing.py'; cases.push(changed)
  changed = structuredClone(reviews); changed[0].review.review_incomplete = true; cases.push(changed)
  for (const value of cases) assert.equal((await childExecutedScript(children, value, workspace)).passed, false)
})

test('merely mentioning a script, inline code and external symlink do not prove execution', async t => {
  const { workspace, children, reviews } = await execution(t)
  for (const command of ['echo .openharness-tmp/reconcile.py', 'python3 -c "print(415)" .openharness-tmp/reconcile.py',
    'python3 .openharness-tmp/reconcile.py; echo done']) {
    reviews[0].review.commands[0].command = command
    assert.equal((await childExecutedScript(children, reviews, workspace)).passed, false)
  }
  await writeFile(join(workspace, 'outside.py'), 'print(415)\n')
  await symlink(join(workspace, 'outside.py'), join(workspace, '.openharness-tmp/link.py'))
  reviews[0].review.commands[0].command = 'python3 .openharness-tmp/link.py'
  assert.equal((await childExecutedScript(children, reviews, workspace)).passed, false)
})
