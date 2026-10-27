import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { INPUTS } from './fixture.mjs'

// Synthetic saved evidence for unit tests, never copied from a real model run.
// The numbers and the saved script are derived from the checked-in input data;
// the host events, original score and usage below are deliberately fabricated.
export const ORIGINAL_CHECK_NAMES = [
  'valid_result_json', 'correct_refund_ids', 'applied_refund', 'net_received', 'unpaid_before_refunds',
  'correction_cited', 'source_files_cited', ...Object.keys(INPUTS).map(name => `unchanged:${name}`),
  'actual_compaction_covers_correction_before_delivery', 'one_child_agent', 'child_read_parent_history',
  'child_executed_command', 'all_agents_successful',
]

function calculate(sources) {
  const rows = text => {
    const [header, ...lines] = text.trim().split('\n').map(line => line.split(','))
    return lines.map(line => Object.fromEntries(header.map((key, index) => [key, line[index]])))
  }
  const orders = rows(sources['orders.csv'])
  const payments = [...new Map(rows(sources['payments.csv']).map(row => [row.payment_id, row])).values()]
    .filter(row => row.status === 'paid' && orders.some(order => order.order_id === row.order_id))
  const refunds = rows(sources['refunds.csv']).filter(row => (row.refund_id === 'R303' || row.status === 'settled')
    && payments.some(payment => payment.payment_id === row.payment_id && payment.order_id === row.order_id))
  const applied_refund = refunds.reduce((sum, row) => sum + Number(row.amount), 0)
  const paid = order => payments.filter(row => row.order_id === order.order_id).reduce((sum, row) => sum + Number(row.amount), 0)
  return {
    refund_ids: refunds.map(row => row.refund_id).sort(), applied_refund,
    net_received: orders.reduce((sum, order) => sum + paid(order), 0) - applied_refund,
    unpaid_before_refunds: orders.reduce((sum, order) => sum + Math.max(Number(order.amount) - paid(order), 0), 0),
  }
}

export async function createAuditFixture(directory) {
  const workspace = join(directory, 'workspace')
  await Promise.all(['outputs', '.openharness-tmp'].map(name => mkdir(join(workspace, name), { recursive: true })))
  const json = (name, value) => writeFile(join(directory, name), JSON.stringify(value, null, 2))
  const rootId = 'synthetic-parent', childId = 'synthetic-child', commandId = 'synthetic-command'
  const script = '.openharness-tmp/reconcile.mjs'
  const initial = `读取 ${Object.keys(INPUTS).join('、')}，保留原文件，并交付 outputs/continuity.json。`
  const correction = 'CORRECTION-ORCHID：R303 已经人工核实为 settled，保留来源文件中的原始状态。'
  const delivered = { ...calculate(INPUTS), evidence: [
    'orders.csv、payments.csv 和 rules.md：P202 重复只计一次；P205 无匹配订单；P206 failed、P207 cancelled 均排除。',
    'refunds.csv 和 rules.md：R301、R302、R303 匹配有效付款；R304 无匹配付款，排除。',
    'event-4: “R303 已经人工核实为 settled”。',
  ] }
  const output = JSON.stringify(delivered, null, 2)
  const tool = (id, tool_name, value) => ({ id, role: 'tool', tool_name, status: 'complete', is_error: false, text: JSON.stringify(value) })
  const sourceReads = prefix => Object.entries(INPUTS).map(([path, content], index) =>
    tool(`${prefix}-source-${index}`, 'read_file', { path, content, next_offset: null }))
  const user = (id, text) => ({ id, role: 'user', status: 'complete', text, truncated: false })
  const sourceMessages = [user('event-0', initial), user('event-4', correction)]
  const usage = { input_tokens: 300000, output_tokens: 10000, responses: 20 }
  const runs = [
    { id: rootId, status: 'waiting_for_input', error: null, compacting: false, workspace_path: workspace,
      context: { observed_usage: { input_tokens: 240000, output_tokens: 8000, responses: 16 }, compaction_count: 1 },
      messages: [...sourceMessages, ...sourceReads('parent'),
        tool('parent-readback', 'read_file', { path: 'outputs/continuity.json', content: output, next_offset: null })] },
    { id: childId, status: 'completed', error: null, compacting: false, workspace_path: workspace,
      context: { observed_usage: { input_tokens: 60000, output_tokens: 2000, responses: 4 }, compaction_count: 0 },
      messages: [tool('child-history', 'agent_control', { agent_id: rootId, messages: sourceMessages }), ...sourceReads('child'),
        tool('child-write', 'apply_patch', { applied: true, files: [{ path: script, status: 'added' }] }),
        tool('child-execution', 'run_command', { command_id: commandId, status: 'completed', exit_code: 0,
          output: JSON.stringify(calculate(INPUTS)) })] },
  ]
  const reviews = runs.map(run => ({ run_id: run.id, review: {
    busy: false, review_incomplete: false, approvals: [], commands: run.id === rootId ? [] : [{
      id: commandId, command: `node ${script}`, status: 'completed', exit_code: 0, network_access: 'denied',
    }],
  } }))
  await Promise.all([
    ...Object.entries(INPUTS).map(([name, content]) => writeFile(join(workspace, name), content)),
    writeFile(join(workspace, 'outputs/continuity.json'), output),
    writeFile(join(workspace, script), `import { readFile } from 'node:fs/promises'\n${calculate.toString()}\n`
      + `const sources = Object.fromEntries(await Promise.all(${JSON.stringify(Object.keys(INPUTS))}.map(async name => [name, await readFile(name, 'utf8')])));\n`
      + 'console.log(JSON.stringify(calculate(sources)));\n'),
    json('result.json', { passed: true, failure: null, run_id: rootId, correction_sequence: 4,
      checks: ORIGINAL_CHECK_NAMES.map(name => ({ name, passed: true })), passed_checks: 16, total_checks: 16,
      observed_usage: usage, duration_ms: 120000 }),
    json('transcripts.json', runs), json('workspace-reviews.json', reviews),
    json('prompts.json', [{ phase: 'initial', message: initial }, { phase: 'correction', message: correction }]),
    json('checkpoints-before-final.json', { correction_sequence: 4, correction_message_id: 'event-4',
      checkpoint_covers_correction: true, checkpoints: [{ id: `context:${rootId}:4:synthetic`, summary: correction }] }),
    // Missing reasoning metadata intentionally exercises historical compatibility.
    json('metadata.json', { model: 'synthetic-test-model', protocol: 'responses', context_window: 32768,
      max_output_tokens: 4096, limits: { observed_tokens: 450000 }, backend_sha256: 'synthetic-backend' }),
  ])
  return directory
}
