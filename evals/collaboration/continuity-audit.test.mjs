import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { auditSavedRun, childWroteExecutedScript, exactCorrectionQuote, noOutstandingCommands, readFileEvidence, recalculate } from './continuity-audit.mjs'
import { compareAudits } from './continuity-compare.mjs'
import { INPUTS } from './fixture.mjs'
import { createAuditFixture, ORIGINAL_CHECK_NAMES } from './continuity-audit.fixture.mjs'

const tool = (tool_name, output) => ({ role: 'tool', tool_name, status: 'complete', is_error: false, text: JSON.stringify(output) })

async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-audit-test-'))
  t.after(() => rm(directory, { recursive: true, force: true }))
  return createAuditFixture(directory)
}

test('synthetic saved evidence retains all original checks and passes all 26 audit requirements', async t => {
  const report = await auditSavedRun(await fixture(t))
  assert.equal(report.passed, true)
  assert.equal(report.original_score.total_checks, 16)
  assert.deepEqual(report.original_score.check_names, [...ORIGINAL_CHECK_NAMES].sort())
  assert.equal(report.total_checks, 26)
  assert.equal(report.passed_checks, 26)
  assert.deepEqual(report.checks.map(check => check.name), [
    'original_score_passed',
    ...Object.keys(INPUTS).flatMap(name => [`unchanged:${name}`, `parent_read_source:${name}`, `child_read_source:${name}`]),
    'independent_recalculation', 'child_read_original_requirement', 'child_read_original_correction',
    'correction_source_is_real_user_message', 'compaction_covers_correction', 'exact_correction_quote',
    'source_and_record_references', 'child_executed_saved_script', 'child_wrote_script_before_execution',
    'parent_readback_matches_deliverable', 'one_child_and_all_agents_successful',
    'no_outstanding_commands_or_approvals', 'all_recorded_commands_network_denied',
  ])
  assert.equal(report.observed_usage.input_tokens + report.observed_usage.output_tokens, 310000)
  assert.equal(report.reasoning_effort, null, 'Historical missing metadata must not be guessed')
})

test('recalculation reads amounts from sources, filters status and deduplicates IDs', () => {
  assert.deepEqual(recalculate(INPUTS), { refund_ids: ['R301', 'R302', 'R303'], valid_payment_total: 475,
    applied_refund: 60, net_received: 415, unpaid_before_refunds: 225 })
  const changed = { ...INPUTS, 'payments.csv': INPUTS['payments.csv'].replaceAll('P202,O102,80,paid', 'P202,O102,70,paid') }
  assert.equal(recalculate(changed).net_received, 405)
  assert.equal(recalculate(changed).unpaid_before_refunds, 235)
})

test('source reads and final readback require actual full tool-returned content', () => {
  const message = tool('read_file', { path: 'orders.csv', content: INPUTS['orders.csv'], next_offset: null })
  assert(readFileEvidence({ messages: [message] }, 'orders.csv', INPUTS['orders.csv']))
  for (const changed of [{ ...message, is_error: true }, { ...message, role: 'assistant' },
    tool('read_file', { path: 'orders.csv', content: INPUTS['orders.csv'], next_offset: 10 }),
    tool('read_file', { path: 'wrong.csv', content: INPUTS['orders.csv'] })]) {
    assert.equal(readFileEvidence({ messages: [changed] }, 'orders.csv', INPUTS['orders.csv']), false)
  }
  assert.equal(readFileEvidence({ messages: [tool('read_file', { path: 'outputs/continuity.json', content: '{"net_received": 414}' })] },
    'outputs/continuity.json', { net_received: 415 }, { json: true }), false)
})

test('correction cites a real exact quote in the same source entry', () => {
  const source = 'CORRECTION-ORCHID：R303 已经人工核实为 settled。'
  assert(exactCorrectionQuote(['event-4: “R303 已经人工核实为 settled”'], source, 4))
  for (const evidence of [['event-4 R303 settled'], ['event-4: “R303 已核实为 settled”'],
    ['event-40: “R303 已经人工核实为 settled”'], ['event-4', '“R303 已经人工核实为 settled”']]) {
    assert.equal(exactCorrectionQuote(evidence, source, 4), false)
  }
})

test('child must create the executed script before its successful execution', () => {
  const execution = { passed: true, run_id: 'child', command_id: 'command-1', script: '.openharness-tmp/calc.py' }
  const write = tool('apply_patch', { applied: true, files: [{ path: execution.script, status: 'added' }] })
  const run = tool('run_command', { command_id: 'command-1', status: 'completed', exit_code: 0 })
  assert(childWroteExecutedScript([{ id: 'child', messages: [write, run] }], execution))
  assert.equal(childWroteExecutedScript([{ id: 'child', messages: [run, write] }], execution), false)
  assert.equal(childWroteExecutedScript([{ id: 'child', messages: [run] }, { id: 'parent', messages: [write] }], execution), false)
})

test('busy review, pending command, approval and missing review cannot claim cleanup', () => {
  const runs = [{ id: 'parent' }, { id: 'child' }]
  const reviews = runs.map(run => ({ run_id: run.id, review: { busy: false, review_incomplete: false, commands: [], approvals: [] } }))
  assert(noOutstandingCommands(runs, reviews))
  assert.equal(noOutstandingCommands(runs, reviews.slice(1)), false)
  for (const change of [{ busy: true }, { review_incomplete: true }, { commands: [{ status: 'running' }] }, { approvals: [{}] }]) {
    const changed = structuredClone(reviews); Object.assign(changed[1].review, change)
    assert.equal(noOutstandingCommands(runs, changed), false)
  }
})

test('tampered output fails even if saved original score still claims success', async t => {
  const target = await fixture(t)
  const output = join(target, 'workspace/outputs/continuity.json')
  const value = JSON.parse(await readFile(output, 'utf8')); value.net_received = 414
  await writeFile(output, JSON.stringify(value))
  const report = await auditSavedRun(target)
  assert.equal(report.passed, false)
  assert(report.checks.some(check => check.name === 'independent_recalculation' && !check.passed))
  assert(report.checks.some(check => check.name === 'parent_readback_matches_deliverable' && !check.passed))
})

test('comparison reports observed savings separately from quality and condition failures', async t => {
  const original = await auditSavedRun(await fixture(t))
  const candidate = structuredClone(original)
  candidate.directory = '/candidate'; candidate.observed_usage = { input_tokens: 180000, output_tokens: 20000, responses: 20 }
  const compared = compareAudits(original, [candidate, candidate])
  assert.equal(compared.observed_summary.candidate_mean_tokens, 200000)
  assert.equal(compared.automatic_quality_retained, true)
  assert.equal(compared.reasoning_settings_verified, false)
  candidate.passed = false; candidate.prompt_sha256 = 'changed'; candidate.limits.observed_tokens = 220000
  const rejected = compareAudits(original, [candidate])
  assert.equal(rejected.automatic_quality_retained, false)
  assert.equal(rejected.recorded_conditions_match, false)
  assert(rejected.candidates[0].condition_differences.includes('limits'))
  assert(rejected.candidates[0].condition_differences.includes('prompt_sha256'))
  candidate.observed_usage.input_tokens = null
  assert.equal(compareAudits(original, [candidate]).observed_summary, null)
})
