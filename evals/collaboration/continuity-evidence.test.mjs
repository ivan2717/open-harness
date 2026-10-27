import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { childReadCorrection, correctionCited, childExecutedScript } from './continuity-evidence.mjs'

const correction = 'CORRECTION-ORCHID：R303 已核实为 settled，来源保持不变。'
const tool = (tool_name, output) => ({ role: 'tool', status: 'complete', is_error: false, tool_name, text: JSON.stringify(output) })
const history = (agent_id = 'root', id = 'event-4') => tool('agent_control', { agent_id,
  messages: [{ id, role: 'user', status: 'complete', text: correction, truncated: false }] })

test('parent alias or raw ID is judged by the same returned source, not request spelling', () => {
  assert(childReadCorrection([{ id: 'child', messages: [history()] }], 'root', 4, correction))
  assert(childReadCorrection([{ id: 'child', messages: [tool('agent_control', {
    agent_id: 'root', message_id: 'event-4', role: 'user', status: 'complete', text: correction,
    text_offset: 0, total_characters: [...correction].length, truncated: false, next_text_offset: null,
  })] }], 'root', 4, correction))
})

test('complete previews count without redundant reads; streaming and final pages alone do not', () => {
  const complete = history()
  assert(childReadCorrection([{ messages: [complete] }], 'root', 4, correction))
  const streaming = JSON.parse(complete.text); streaming.messages[0].status = 'streaming'
  assert.equal(childReadCorrection([{ messages: [tool('agent_control', streaming)] }], 'root', 4, correction), false)
  const chars = [...correction], split = 12
  const page = (offset, end) => tool('agent_control', { agent_id: 'root', message_id: 'event-4', role: 'user', status: 'complete',
    text: chars.slice(offset, end).join(''), text_offset: offset, total_characters: chars.length,
    truncated: end < chars.length, next_text_offset: end < chars.length ? end : null })
  const first = page(0, split), last = page(split, chars.length)
  assert.equal(childReadCorrection([{ messages: [last] }], 'root', 4, correction), false)
  assert(childReadCorrection([{ messages: [first, last] }], 'root', 4, correction))
  assert.equal(childReadCorrection([{ messages: [first] }, { messages: [last] }], 'root', 4, correction), false)
})

test('complete search preview must identify the actual parent user field', () => {
  const output = { checkpoint_id: 'context:root:9:hash', hits: [{ sequence: 4, event_type: 'input_received', field: '/content',
    preview: correction, preview_offset: 0, total_characters: [...correction].length, preview_truncated: false }] }
  assert(childReadCorrection([{ messages: [tool('search_run_history', output)] }], 'root', 4, correction))
  for (const change of [{ event_type: 'model_response_received' }, { preview_truncated: true }, { field: '/files/0/text' }, { preview_offset: 1 }]) {
    const changed = structuredClone(output); Object.assign(changed.hits[0], change)
    assert.equal(childReadCorrection([{ messages: [tool('search_run_history', changed)] }], 'root', 4, correction), false)
  }
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

test('the temporary directory itself cannot be a symlink outside the workspace', async t => {
  const { workspace, children, reviews } = await execution(t)
  const external = await mkdtemp(join(tmpdir(), 'openharness-external-script-'))
  t.after(() => rm(external, { recursive: true, force: true }))
  await writeFile(join(external, 'reconcile.py'), 'print(415)\n')
  await rm(join(workspace, '.openharness-tmp'), { recursive: true })
  await symlink(external, join(workspace, '.openharness-tmp'))
  assert.equal((await childExecutedScript(children, reviews, workspace)).passed, false)
})
