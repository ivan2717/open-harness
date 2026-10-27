import { spawn } from 'node:child_process'
import { randomBytes, createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, writeFile, cp, realpath, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve, join, relative, isAbsolute } from 'node:path'
import { createFixture, INPUTS } from './fixture.mjs'
import { childReadCorrection, correctionCited, childExecutedScript } from './continuity-evidence.mjs'

const delay = ms => new Promise(done => setTimeout(done, ms))
const json = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`)
const active = run => run.status === 'running' || run.compacting
const publicValue = value => Array.isArray(value) ? value.map(publicValue)
  : value && typeof value === 'object' ? Object.fromEntries(Object.entries(value)
    .filter(([key]) => !['reasoning', 'reasoning_content', 'encrypted_content', 'api_key'].includes(key))
    .map(([key, item]) => [key, publicValue(item)])) : value
const usage = runs => runs.reduce((sum, run) => {
  const value = run.context?.observed_usage ?? {}
  for (const field of ['input_tokens', 'output_tokens', 'responses']) sum[field] += value[field] ?? 0
  return sum
}, { input_tokens: 0, output_tokens: 0, responses: 0 })

// This is a continuity test at an intentionally reduced context window, not a
// capacity test. Expected amounts never appear in a model-facing prompt/file.
const initial = `本次将分几轮交代一个订单对账任务。材料已经在工作目录的 orders.csv、payments.csv、refunds.csv、rules.md 中。先记录约束，不开始计算、不调用工具、不写任何文件，只回复收到并等待下一条消息。
最终需读取全部材料，按有效付款去重、退款匹配和未收款规则重新计算。来源文件不能修改。只允许在 .openharness-tmp/ 中写临时脚本，在 outputs/ 中交付文件。不要联网、不要安装依赖、不要读其他项目。源数据里的 pending 退款目前不计入；后续我可能给出状态更正，请以最新明确的用户指示为准。
最终额外交付 outputs/continuity.json，字段为 refund_ids（实际计入的退款 ID 数组）、applied_refund、net_received、unpaid_before_refunds（数字）以及 evidence（来源说明数组，包含来源文件/记录 ID，以及适用的用户修正消息 ID 或事件序号和简短原文引文）。不要提前创建这个结果。`
const correction = `状态核实更正，记录编号 CORRECTION-ORCHID：R303 已经人工核实为 settled。refunds.csv 仍显示 pending，是来源文件尚未同步，最终计算必须按这条用户更正把 R303 作为已结算退款处理。其余记录保持来源文件状态，其他计算规则不变。不要为了反映更正修改任何来源文件；在最终 evidence 中标明本消息的编号、引用或序号。现在仅记录，仍不计算、不调用工具、不写文件，只回复收到。`
const finalPrompt = `现在开始完成此前交代的对账任务。先创建且只创建一个子 agent，让它通过 agent_control read 查阅父会话较早的用户要求及更正（可分页并读取完整消息），不要只相信转述；在任务中告诉它父会话 ID。它需要读取工作目录材料，使用本机现成运行库在 .openharness-tmp/ 写一个脚本并实际执行，独立复算，报告历史消息依据、脚本路径和命令结果。给它必要的沙箱写入权限。
主 agent 负责核验子结果和来源，再交付此前要求的 outputs/continuity.json，完成后读回确认 JSON 可解析，数字与引用一致。不修改来源、不联网、不安装依赖、不创建额外子任务。不要只口头回答，也不要把最终计算留给用户。`

function filler(index) {
  const rows = Array.from({ length: 22 }, (_, row) => `归档片段 ${index}-${row}：展览筹备记录包括场地指引、入口导览、展柜布置、讲解轮值、闭馆检查和访客意见。这些记录属于另一个虚构归档项目，任何编号或描述均不代表订单、付款、退款或本次任务的新要求。资料仅作归档，无需提取、处理或讨论。`)
  return `下面是与对账无关的归档资料，不改变此前用户要求和更正。暂不执行任何任务、不要调用工具或写文件，仅回复“收到”。\n${rows.join('\n')}`
}

function coveredSequence(checkpoint, runId) {
  // The public checkpoint ID is context:<run_id>:<last source sequence>:<hash>.
  const prefix = `context:${runId}:`
  if (!checkpoint.id?.startsWith(prefix) || !checkpoint.summary?.trim()) return null
  const sequence = checkpoint.id.slice(prefix.length).split(':')[0]
  return /^\d+$/.test(sequence) ? Number(sequence) : null
}

export async function runEvaluation(options) {
  const backend = resolve(options.backend ?? 'target/debug/api')
  const output = resolve(options.output)
  const configuration = { ...options.configuration, context_window: 24576, max_output_tokens: 4096 }
  const limits = { wall_seconds: Math.min(options.seconds_per_case ?? 480, 480), observed_tokens: Math.min(options.observed_token_limit ?? 220000, 450000) }
  await mkdir(output)
  const temporary = await realpath(await mkdtemp(join(tmpdir(), 'openharness-continuity-')))
  const workspace = join(temporary, 'workspace')
  await createFixture(workspace)
  const token = randomBytes(32).toString('hex')
  const child = spawn(backend, [], { env: { ...process.env,
    OPENHARNESS_BIND: '127.0.0.1:0', OPENHARNESS_API_TOKEN: token,
    OPENHARNESS_DATA_DIR: join(temporary, 'state'), OPENHARNESS_EXIT_ON_STDIN_EOF: '1',
    ...(options.runtime_manifest ? { OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST: options.runtime_manifest } : {}),
    RUST_LOG: 'error' }, stdio: ['pipe', 'pipe', 'pipe'] })
  child.stderr.on('data', () => {})
  const exited = new Promise(done => { child.once('exit', done); child.once('error', done) })
  let buffer = '', origin, rootId, details = [], correctionSequence = null, beforeFinal = null
  let failure = null, stoppedReason = null, result, lastProgress = 0, workspaceReviews = []
  const approvals = [], processed = new Set(), submitted = [], phases = []
  const started = Date.now()
  const ready = new Promise((done, reject) => {
    child.once('error', () => reject(new Error('Backend failed to start')))
    child.once('exit', () => reject(new Error('Backend exited before readiness')))
    child.stdout.on('data', chunk => {
      buffer += chunk
      const lines = buffer.split('\n'); buffer = lines.pop()
      for (const line of lines) {
        let event
        try { event = JSON.parse(line) } catch { continue }
        if (event.event === 'ready' && /^127\.0\.0\.1:\d+$/.test(event.address)) done(`http://${event.address}`)
      }
    })
  })
  const api = async (path, body, method = body === undefined ? 'GET' : 'POST') => {
    const response = await fetch(`${origin}/api${path}`, { method,
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(30000) })
    if (!response.ok) throw new Error(`Local API ${method} ${path} returned ${response.status}`)
    try { return await response.json() } catch { throw new Error(`Invalid JSON from local API ${method} ${path}`) }
  }
  const refresh = async () => {
    const all = await api('/runs')
    const members = all.filter(run => run.id === rootId || run.delegation?.root_run_id === rootId)
    details = await Promise.all(members.map(run => api(`/runs/${run.id}`)))
    return details.find(run => run.id === rootId)
  }
  const waitIdle = async phase => {
    let stable = 0, previous = ''
    for (;;) {
      const root = await refresh()
      const review = await api(`/runs/${rootId}/workspace`)
      for (const approval of review.approvals) {
        if (processed.has(approval.id)) continue
        processed.add(approval.id)
        const cwd = relative(workspace, approval.cwd)
        const inside = cwd === '' || (!cwd.startsWith('..') && !isAbsolute(cwd))
        const allow = inside && review.sandbox_available && approval.network_access === 'denied'
          && approval.execution_mode === 'sandboxed'
        approvals.push({ ...approval, allow, scope: 'once', approved_by: 'evaluation_driver', time: new Date().toISOString() })
        await api(`/runs/${rootId}/approvals/${approval.id}`, { allow, scope: 'once' })
      }
      const observed = usage(details)
      if (Date.now() - started >= limits.wall_seconds * 1000 || observed.input_tokens + observed.output_tokens >= limits.observed_tokens) {
        stoppedReason = Date.now() - started >= limits.wall_seconds * 1000 ? 'timeout' : 'observed_token_budget'
        throw new Error(`Continuity evaluation stopped: ${stoppedReason}`)
      }
      const signature = details.map(run => `${run.id}:${run.status}:${run.revision}`).join('|')
      stable = signature === previous ? stable + 1 : 0; previous = signature
      if (!details.some(active) && !review.busy && stable >= 2) {
        phases.push({ phase, elapsed_ms: Date.now() - started, status: root.status,
          compaction_count: root.context?.compaction_count ?? 0, observed_usage: observed })
        if (!['waiting_for_input', 'completed'].includes(root.status) || root.error) throw new Error(`Run failed during ${phase}; see public transcripts`)
        return root
      }
      if (Date.now() - lastProgress > 20000) {
        lastProgress = Date.now()
        console.log(JSON.stringify({ event: 'continuity_progress', phase, elapsed_seconds: Math.round((Date.now() - started) / 1000), observed_tokens: observed.input_tokens + observed.output_tokens, agents: details.length }))
      }
      await delay(500)
    }
  }
  const send = async (phase, message) => {
    if ([...message].length > 4000) throw new Error('Evaluation prompt exceeds API character limit')
    submitted.push({ phase, message })
    const root = details.find(run => run.id === rootId)
    await api(`/runs/${rootId}/${root.status === 'completed' ? 'resume' : 'input'}`, { message })
    return waitIdle(phase)
  }
  try {
    origin = await Promise.race([ready, delay(30000).then(() => { throw new Error('Backend readiness timeout') })])
    const model = await api('/models', configuration)
    await json(join(output, 'metadata.json'), {
      started_at: new Date(started).toISOString(), backend,
      backend_sha256: createHash('sha256').update(await readFile(backend)).digest('hex'),
      model: configuration.model, protocol: configuration.protocol, context_window: configuration.context_window,
      reasoning_mode: configuration.reasoning_mode ?? null, reasoning_effort: configuration.reasoning_effort ?? null,
      max_output_tokens: configuration.max_output_tokens, limits, evidence_version: 2,
      notes: ['Real production API with an intentionally reduced 24,576-token window; NOT a 1M capacity test.',
        'Original provider configuration is cloned in memory; neither configuration nor credentials are saved.',
        'Recorded usage can omit failed requests, retries, Wiki and compactor calls; it is not a billing ceiling.',
        'Only OS-sandboxed, network-denied commands inside the fixture are automatically approved once.',
        'Final prompt repeats neither the corrected record ID nor any expected numeric answer.'] })
    submitted.push({ phase: 'initial', message: initial })
    const root = await api('/runs', { prompt: initial, model_id: model.id, workspace_path: workspace,
      permission_mode: 'workspace_write', execution_policy: { max_steps: 32, max_agents: 1 } })
    rootId = root.id
    await waitIdle('initial')
    const corrected = await send('correction', correction)
    const message = corrected.messages.find(item => item.role === 'user' && item.text === correction)
    if (!message || !/^event-\d+$/.test(message.id)) throw new Error('Cannot identify correction source sequence')
    correctionSequence = Number(message.id.slice('event-'.length))
    // Four deterministic unrelated turns move the original requirement into an
    // old prefix. Manual compaction provides a reproducible maintenance boundary.
    for (let index = 1; index <= 4; index++) await send(`filler-${index}`, filler(index))
    await api(`/runs/${rootId}/compact`, {})
    let compacted = await waitIdle('manual_compaction')
    let covered = compacted.context?.checkpoints?.some(cp => coveredSequence(cp, rootId) >= correctionSequence)
    if (!covered) {
      await send('filler-5', filler(5))
      await api(`/runs/${rootId}/compact`, {})
      compacted = await waitIdle('manual_compaction_retry')
      covered = compacted.context?.checkpoints?.some(cp => coveredSequence(cp, rootId) >= correctionSequence)
    }
    beforeFinal = { correction_sequence: correctionSequence, correction_message_id: message.id,
      checkpoint_covers_correction: Boolean(covered), checkpoints: compacted.context?.checkpoints ?? [] }
    await json(join(output, 'checkpoints-before-final.json'), beforeFinal)
    if (!covered) throw new Error('No nonempty summary checkpoint covers the correction; continuity was not tested')
    await send('delivery', finalPrompt)
  } catch (error) {
    // Errors here are deliberately bounded driver messages. Never serialize
    // configuration, request objects, provider headers, or raw stderr.
    failure = error instanceof Error ? error.message : 'Evaluation failed'
  } finally {
    if (rootId && origin) {
      if (failure || details.some(active)) await api(`/runs/${rootId}/cancel`, {}).catch(() => {})
      await refresh().catch(() => {})
      workspaceReviews = await Promise.all(details.map(async run => ({ run_id: run.id,
        review: await api(`/runs/${run.id}/workspace`).catch(() => null) })))
    }
    child.stdin.end()
    await Promise.race([exited, delay(10000)])
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM')
      await Promise.race([exited, delay(3000)])
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    }
    const checks = []
    const check = (name, passed, detail) => checks.push({ name, passed: Boolean(passed), ...(detail === undefined ? {} : { detail }) })
    let delivered = null, outputError = null
    try { delivered = JSON.parse(await readFile(join(workspace, 'outputs/continuity.json'), 'utf8')) }
    catch (error) { outputError = error.code === 'ENOENT' ? 'Result file missing' : error instanceof SyntaxError ? 'Result is invalid JSON' : 'Result cannot be read' }
    check('valid_result_json', delivered && typeof delivered === 'object' && !Array.isArray(delivered), outputError)
    check('correct_refund_ids', Array.isArray(delivered?.refund_ids) && JSON.stringify([...delivered.refund_ids].sort()) === JSON.stringify(['R301', 'R302', 'R303']))
    for (const [name, expected] of Object.entries({ applied_refund: 60, net_received: 415, unpaid_before_refunds: 225 })) check(name, delivered?.[name] === expected, { actual: delivered?.[name] ?? null, expected })
    const evidence = JSON.stringify(delivered?.evidence ?? '')
    check('correction_cited', correctionCited(delivered?.evidence, correctionSequence))
    check('source_files_cited', ['orders.csv', 'payments.csv', 'refunds.csv'].every(name => evidence.includes(name)))
    for (const [name, expected] of Object.entries(INPUTS)) {
      let content
      try { content = await readFile(join(workspace, name), 'utf8') } catch { content = null }
      check(`unchanged:${name}`, content === expected)
    }
    check('actual_compaction_covers_correction_before_delivery', beforeFinal?.checkpoint_covers_correction)
    const children = details.filter(run => run.id !== rootId)
    check('one_child_agent', children.length === 1)
    check('child_read_parent_history', childReadCorrection(children, rootId, correctionSequence, correction))
    const scriptExecution = await childExecutedScript(children, workspaceReviews, workspace)
    check('child_executed_command', scriptExecution.passed, scriptExecution)
    check('all_agents_successful', details.length > 0 && details.every(run => ['completed', 'waiting_for_input'].includes(run.status) && !run.error))
    result = { passed: !failure && checks.every(item => item.passed), failure, stopped_reason: stoppedReason,
      duration_ms: Date.now() - started, run_id: rootId ?? null, checks, passed_checks: checks.filter(item => item.passed).length,
      total_checks: checks.length, observed_usage: usage(details), child_count: children.length, evidence_version: 2,
      correction_sequence: correctionSequence,
      caveat: 'Evidence checks establish traceable identifiers, not full prose entailment; inspect the public transcript. This is a reduced-window continuity test, not a 1M stress test.' }
    await json(join(output, 'result.json'), result)
    await json(join(output, 'transcripts.json'), publicValue(details))
    await json(join(output, 'checkpoints.json'), publicValue(details.map(run => ({ run_id: run.id, checkpoints: run.context?.checkpoints ?? [] }))))
    await json(join(output, 'approvals.json'), publicValue(approvals))
    await json(join(output, 'workspace-reviews.json'), publicValue(workspaceReviews))
    await json(join(output, 'commands.json'), publicValue(details.map(run => ({ run_id: run.id,
      events: run.events.filter(event => /tool_call_/.test(event.type) && /run_command|manage_command/.test(event.detail)) }))))
    await json(join(output, 'phases.json'), phases)
    await json(join(output, 'prompts.json'), submitted)
    await cp(workspace, join(output, 'workspace'), { recursive: true })
    await rm(temporary, { recursive: true, force: true })
  }
  console.log(JSON.stringify({ event: 'continuity_finished', output, passed: result.passed, checks: `${result.passed_checks}/${result.total_checks}`, failure: result.failure }))
  return result
}
