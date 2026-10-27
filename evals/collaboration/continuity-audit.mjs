// Read-only review of saved evidence. Never executes a model-produced script or
// repairs an output. The original run score remains a separate, required gate.
import { readFile, realpath } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { isDeepStrictEqual } from 'node:util'
import { isAbsolute, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { INPUTS } from './fixture.mjs'
import { childReadCorrection, childExecutedScript, correctionCited } from './continuity-evidence.mjs'

const parse = text => { try { return JSON.parse(text) } catch { return null } }
const successful = run => (run?.messages ?? []).filter(message => message.role === 'tool'
  && message.status === 'complete' && message.is_error === false)
const sha256 = value => createHash('sha256').update(value).digest('hex')
const localPath = (path, root) => typeof path === 'string'
  ? (isAbsolute(path) ? relative(root ?? '', path) : path.replace(/^\.\//, '')) : null
async function safeRead(base, name) {
  const path = await realpath(join(base, name)), fromBase = relative(base, path)
  if (!fromBase || fromBase === '..' || fromBase.startsWith(`..${sep}`) || isAbsolute(fromBase)) {
    throw new Error('Saved evidence path resolves outside its evidence directory')
  }
  return readFile(path, 'utf8')
}

function csv(text) {
  // These fixed inputs deliberately contain no quoted cells or embedded commas.
  const [header, ...rows] = text.trim().split(/\r?\n/).map(line => line.split(','))
  return rows.map(row => Object.fromEntries(header.map((name, index) => [name, row[index]])))
}

export function recalculate(sources) {
  const orders = new Map(csv(sources['orders.csv']).map(row => [row.order_id, Number(row.amount)]))
  const seen = new Set(), valid = new Map(), paid = new Map([...orders.keys()].map(id => [id, 0]))
  for (const row of csv(sources['payments.csv'])) {
    if (seen.has(row.payment_id)) continue
    seen.add(row.payment_id)
    if (row.status !== 'paid' || !orders.has(row.order_id)) continue
    valid.set(row.payment_id, row)
    paid.set(row.order_id, paid.get(row.order_id) + Number(row.amount))
  }
  const refunds = csv(sources['refunds.csv']).filter(row => {
    const status = row.refund_id === 'R303' ? 'settled' : row.status
    return status === 'settled' && valid.get(row.payment_id)?.order_id === row.order_id
  })
  const applied_refund = refunds.reduce((sum, row) => sum + Number(row.amount), 0)
  const valid_payment_total = [...paid.values()].reduce((sum, amount) => sum + amount, 0)
  return { refund_ids: refunds.map(row => row.refund_id).sort(), valid_payment_total, applied_refund,
    net_received: valid_payment_total - applied_refund,
    unpaid_before_refunds: [...orders].reduce((sum, [id, amount]) => sum + Math.max(amount - paid.get(id), 0), 0) }
}

export function readFileEvidence(run, path, expected, { json = false } = {}) {
  return successful(run).some(message => {
    if (message.tool_name !== 'read_file') return false
    const output = parse(message.text)
    if (localPath(output?.path, run.workspace_path) !== path || output.truncated === true
      || output.next_offset != null || output.next_cursor != null) return false
    return json ? parse(output.content) !== null && isDeepStrictEqual(parse(output.content), expected)
      : output.content === expected
  })
}

export function noOutstandingCommands(runs, reviews) {
  return runs.length > 0 && runs.every(run => {
    const matches = reviews.filter(item => item.run_id === run.id)
    if (matches.length !== 1) return false
    const review = matches[0].review
    return review?.review_incomplete === false && review.busy === false
      && Array.isArray(review.commands) && review.commands.every(command => command.status === 'completed')
      && Array.isArray(review.approvals) && review.approvals.length === 0
  })
}

export function exactCorrectionQuote(evidence, correction, sequence) {
  if (!Array.isArray(evidence) || !evidence.every(item => typeof item === 'string')) return false
  // An exact, meaningful substring of the user correction must accompany its
  // record/event citation in the same evidence entry. A bare record ID is weak.
  return evidence.some(entry => correctionCited(entry, sequence) &&
    [...entry.matchAll(/[“「『‘"`]([^“”「」『』‘’"`]+)[”」』’"`]/gu)].some(match => {
      const quote = match[1].trim()
      return quote.length >= 8 && /R303/.test(quote) && /settled/.test(quote) && correction.includes(quote)
    }))
}

async function executedScript(children, reviews, workspace) {
  // Saved commands can refer to the original now-deleted fixture directory.
  // Relocate only that exact reviewed workspace, never an arbitrary path.
  const mapped = reviews.map(item => {
    const run = children.find(run => run.id === item.run_id)
    if (!run?.workspace_path || !item.review) return item
    return { ...item, review: { ...item.review, commands: (item.review.commands ?? []).map(command => ({
      ...command, command: command.command.replaceAll(`${run.workspace_path}/`, `${workspace}/`),
    })) } }
  })
  return childExecutedScript(children, mapped, workspace)
}

export function childWroteExecutedScript(children, execution) {
  if (!execution.passed) return false
  const child = children.find(run => run.id === execution.run_id)
  const messages = successful(child)
  const commandIndex = messages.findIndex(message => ['run_command', 'manage_command'].includes(message.tool_name)
    && parse(message.text)?.command_id === execution.command_id)
  return messages.slice(0, commandIndex).some(message => {
    if (message.tool_name !== 'apply_patch') return false
    const output = parse(message.text)
    return output?.applied === true && output.files?.some(file => file.status === 'added'
      && localPath(file.path, child.workspace_path) === execution.script)
  })
}

export async function auditSavedRun(directory) {
  directory = await realpath(directory)
  const load = async name => JSON.parse(await safeRead(directory, name))
  const [result, runs, reviews, prompts, checkpoint, metadata] = await Promise.all([
    load('result.json'), load('transcripts.json'), load('workspace-reviews.json'),
    load('prompts.json'), load('checkpoints-before-final.json'), load('metadata.json'),
  ])
  const workspace = await realpath(join(directory, 'workspace')), workspacePath = relative(directory, workspace)
  if (!workspacePath || workspacePath === '..' || workspacePath.startsWith(`..${sep}`) || isAbsolute(workspacePath)) {
    throw new Error('Saved workspace resolves outside its evidence directory')
  }
  const root = runs.find(run => run.id === result.run_id)
  const children = runs.filter(run => run.id !== result.run_id)
  const correction = prompts.find(item => item.phase === 'correction')?.message ?? ''
  const initial = prompts.find(item => item.phase === 'initial')?.message ?? ''
  const delivered = parse(await safeRead(workspace, 'outputs/continuity.json').catch(() => ''))
  const checks = []
  const check = (name, passed, detail) => checks.push({ name, passed: Boolean(passed), ...(detail == null ? {} : { detail }) })
  check('original_score_passed', result.passed === true && result.failure === null && result.checks?.length >= 16
    && result.checks.every(check => check.passed === true))
  const sources = {}, source_sha256 = {}
  for (const [name, expected] of Object.entries(INPUTS)) {
    sources[name] = await safeRead(workspace, name).catch(() => '')
    source_sha256[name] = sha256(sources[name])
    check(`unchanged:${name}`, sources[name] === expected)
    check(`parent_read_source:${name}`, readFileEvidence(root, name, sources[name]))
    check(`child_read_source:${name}`, children.length === 1 && readFileEvidence(children[0], name, sources[name]))
  }
  const calculated = recalculate(sources)
  check('independent_recalculation', delivered && isDeepStrictEqual(delivered.refund_ids?.slice().sort(), calculated.refund_ids)
    && ['applied_refund', 'net_received', 'unpaid_before_refunds'].every(key => typeof delivered[key] === 'number'
      && Number.isFinite(delivered[key]) && delivered[key] === calculated[key]), { calculated })
  check('child_read_original_requirement', Boolean(initial) && childReadCorrection(children, result.run_id, 0, initial))
  check('child_read_original_correction', Boolean(correction) && childReadCorrection(children, result.run_id, result.correction_sequence, correction))
  check('correction_source_is_real_user_message', root?.messages?.some(message => message.id === `event-${result.correction_sequence}`
    && message.role === 'user' && message.text === correction))
  check('compaction_covers_correction', checkpoint.checkpoint_covers_correction === true
    && checkpoint.correction_sequence === result.correction_sequence)
  check('exact_correction_quote', exactCorrectionQuote(delivered?.evidence, correction, result.correction_sequence))
  const evidence = JSON.stringify(delivered?.evidence ?? '')
  check('source_and_record_references', ['orders.csv', 'payments.csv', 'refunds.csv', 'rules.md',
    'P202', 'P205', 'P206', 'P207', 'R301', 'R302', 'R303', 'R304'].every(text => evidence.includes(text)))
  const execution = await executedScript(children, reviews, workspace)
  check('child_executed_saved_script', execution.passed, execution)
  check('child_wrote_script_before_execution', childWroteExecutedScript(children, execution))
  check('parent_readback_matches_deliverable', delivered !== null && readFileEvidence(root, 'outputs/continuity.json', delivered, { json: true }))
  check('one_child_and_all_agents_successful', children.length === 1 && runs.every(run => ['completed', 'waiting_for_input'].includes(run.status)
    && !run.error && !run.compacting))
  check('no_outstanding_commands_or_approvals', noOutstandingCommands(runs, reviews))
  check('all_recorded_commands_network_denied', reviews.every(item => item.review?.commands?.every(command => command.network_access === 'denied')))
  // Inputs are included as digests only; never dump transcripts or credentials.
  return { directory, audit_version: 1, passed: checks.every(check => check.passed), checks,
    passed_checks: checks.filter(check => check.passed).length, total_checks: checks.length,
    original_score: { passed: result.passed, passed_checks: result.passed_checks, total_checks: result.total_checks,
      check_names: (result.checks ?? []).map(check => check.name).sort() },
    source_sha256, prompt_sha256: sha256(JSON.stringify(prompts)), calculated,
    model: metadata.model, protocol: metadata.protocol, context_window: metadata.context_window,
    max_output_tokens: metadata.max_output_tokens, reasoning_effort: metadata.reasoning_effort ?? null,
    reasoning_mode: metadata.reasoning_mode ?? null, limits: metadata.limits, backend_sha256: metadata.backend_sha256,
    observed_usage: result.observed_usage, duration_ms: result.duration_ms,
    by_agent: runs.map(run => ({ role: run.id === result.run_id ? 'parent' : 'child', run_id: run.id,
      observed_usage: run.context?.observed_usage ?? null, compactions: run.context?.compaction_count ?? 0 })),
    manual_review_required: ['Script derives its figures from the actual sources rather than hardcoding answers.',
      'Evidence prose correctly explains deduplication, exclusions and the user override.',
      'Parent actually reconciles the child report against sources; file reads alone do not prove reasoning.'],
    caveat: 'Read-only checks add requirements to the original score; they do not replace semantic review or execute model scripts.' }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 3) throw new Error('Usage: node continuity-audit.mjs SAVED_RUN_DIRECTORY')
  const report = await auditSavedRun(process.argv[2])
  console.log(JSON.stringify(report, null, 2))
  if (!report.passed) process.exitCode = 1
}
