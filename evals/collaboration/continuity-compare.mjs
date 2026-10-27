// Summarize saved runs without mutating evidence or making another model call.
import { isDeepStrictEqual } from 'node:util'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { auditSavedRun } from './continuity-audit.mjs'

const totals = usage => {
  if (!usage || !['input_tokens', 'output_tokens', 'responses'].every(key => Number.isSafeInteger(usage[key]) && usage[key] >= 0)) return null
  return { ...usage, total_tokens: usage.input_tokens + usage.output_tokens }
}
const average = values => values.reduce((sum, value) => sum + value, 0) / values.length
const savings = (baseline, candidate) => baseline > 0 ? (baseline - candidate) / baseline * 100 : null

export function compareAudits(baseline, candidates) {
  if (!candidates.length) throw new Error('At least one candidate run is required')
  const baselineUsage = totals(baseline.observed_usage)
  const fields = ['audit_version', 'model', 'protocol', 'context_window', 'max_output_tokens', 'limits', 'prompt_sha256', 'source_sha256']
  const baselineRoles = Object.fromEntries(baseline.by_agent.map(agent => [agent.role, totals(agent.observed_usage)]))
  const rows = candidates.map(candidate => {
    const usage = totals(candidate.observed_usage)
    const differences = fields.filter(field => !isDeepStrictEqual(baseline[field], candidate[field]))
    if (!isDeepStrictEqual(baseline.original_score.check_names, candidate.original_score.check_names)) differences.push('original_score.check_names')
    const knownReasoning = baseline.reasoning_effort !== null && candidate.reasoning_effort !== null
      && baseline.reasoning_mode !== null && candidate.reasoning_mode !== null
    if (['reasoning_effort', 'reasoning_mode'].some(field => baseline[field] != null && candidate[field] != null
      && baseline[field] !== candidate[field])) differences.push('reasoning_settings')
    return { directory: candidate.directory, automatic_quality_passed: candidate.passed,
      failed_checks: candidate.checks.filter(check => !check.passed).map(check => check.name),
      recorded_conditions_match: differences.length === 0, condition_differences: differences,
      reasoning_settings_verified: knownReasoning && !differences.includes('reasoning_settings'),
      observed_usage: usage, duration_ms: candidate.duration_ms,
      observed_token_reduction_percent: usage && baselineUsage ? savings(baselineUsage.total_tokens, usage.total_tokens) : null,
      by_agent: candidate.by_agent.map(agent => { const value = totals(agent.observed_usage); return {
        role: agent.role, observed_usage: value, compactions: agent.compactions,
        observed_token_reduction_percent: value && baselineRoles[agent.role]
          ? savings(baselineRoles[agent.role].total_tokens, value.total_tokens) : null,
      } }),
      backend_sha256: candidate.backend_sha256 }
  })
  const validUsage = baselineUsage !== null && rows.every(row => row.observed_usage !== null)
  const tokenValues = validUsage ? rows.map(row => row.observed_usage.total_tokens) : []
  return { comparison_version: 1, baseline: { directory: baseline.directory, automatic_quality_passed: baseline.passed,
    observed_usage: baselineUsage, duration_ms: baseline.duration_ms, backend_sha256: baseline.backend_sha256 },
    candidates: rows, candidate_count: rows.length,
    automatic_quality_retained: baseline.passed && rows.every(row => row.automatic_quality_passed),
    recorded_conditions_match: rows.every(row => row.recorded_conditions_match),
    reasoning_settings_verified: rows.every(row => row.reasoning_settings_verified),
    observed_summary: validUsage ? { candidate_mean_tokens: average(tokenValues),
      candidate_min_tokens: Math.min(...tokenValues), candidate_max_tokens: Math.max(...tokenValues),
      mean_reduction_percent: savings(baselineUsage.total_tokens, average(tokenValues)),
      all_candidates_use_fewer_tokens: tokenValues.every(value => value < baselineUsage.total_tokens) } : null,
    manual_review_required: baseline.manual_review_required,
    caveats: ['All original checks remain required; additional audit checks use the same verifier for baseline and candidates.',
      'Automatic quality checks do not replace a review of script logic and evidence prose.',
      'Missing historical reasoning metadata is unverified; consult the original run review and launcher settings.',
      'Observed tokens are recorded usage, not a bill; retries, failed requests, compaction and Wiki usage may be omitted.',
      'These repeated runs of one fixed task are descriptive observations, not statistical significance or a general success-rate estimate.'] }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length < 4) throw new Error('Usage: node continuity-compare.mjs BASELINE_DIRECTORY CANDIDATE_DIRECTORY [CANDIDATE_DIRECTORY ...]')
  const [baseline, ...candidates] = await Promise.all(process.argv.slice(2).map(auditSavedRun))
  const report = compareAudits(baseline, candidates)
  console.log(JSON.stringify(report, null, 2))
  if (!report.automatic_quality_retained || !report.recorded_conditions_match || report.observed_summary === null) process.exitCode = 1
}
