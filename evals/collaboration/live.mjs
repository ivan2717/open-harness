import { spawn } from 'node:child_process'
import { randomBytes, createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, writeFile, cp, realpath } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve, join, relative, isAbsolute } from 'node:path'
import { once } from 'node:events'
import { createFixture, promptFor } from './fixture.mjs'
import { score } from './score.mjs'

const delay = (ms) => new Promise((done) => setTimeout(done, ms))
const json = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`)
const active = (run) => run.status === 'running' || run.compacting
const publicRun = (run) => ({...run, messages:run.messages.map(({reasoning, ...message}) => message)})
const eventCounts = (run, kind) => run.events.filter((event) => event.type === kind).length

export async function runEvaluation(options) {
  const {configuration} = options
  const backend = resolve(options.backend ?? 'target/debug/api')
  const output = resolve(options.output)
  // A repeated command must not overwrite evidence or silently rerun paid work.
  await mkdir(output)
  const temporary = await realpath(await mkdtemp(join(tmpdir(), 'openharness-collaboration-live-')))
  const token = randomBytes(32).toString('hex')
  const child = spawn(backend, [], {env:{...process.env,
    OPENHARNESS_BIND:'127.0.0.1:0', OPENHARNESS_API_TOKEN:token,
    OPENHARNESS_DATA_DIR:join(temporary,'state'), OPENHARNESS_EXIT_ON_STDIN_EOF:'1',
    OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST:options.runtime_manifest,
    RUST_LOG:'error'}, stdio:['pipe','pipe','pipe']})
  // Avoid persisting unstructured runtime diagnostics; the API provides public
  // failure records. No provider credential is written to disk or stdout.
  child.stderr.on('data', () => {})
  const exited = once(child,'exit')
  let origin
  let buffer = ''
  const ready = new Promise((done, reject) => {
    child.once('error', () => reject(new Error('Backend failed to start')))
    child.once('exit', () => reject(new Error('Backend exited before readiness')))
    child.stdout.on('data', (chunk) => {
      buffer += chunk
      const lines=buffer.split('\n'); buffer=lines.pop()
      for (const line of lines) {
        try {
          const event=JSON.parse(line)
          if(event.event==='ready' && /^127\.0\.0\.1:\d+$/.test(event.address)) done(`http://${event.address}`)
        } catch {}
      }
    })
  })
  const api = async (path, body, method = body === undefined ? 'GET' : 'POST') => {
    const response=await fetch(`${origin}/api${path}`, {method,
      headers:{authorization:`Bearer ${token}`, 'content-type':'application/json'},
      ...(body===undefined?{}:{body:JSON.stringify(body)}), signal:AbortSignal.timeout(30_000)})
    const value=await response.json()
    if(!response.ok) throw new Error(`Local API ${method} ${path} returned ${response.status}`)
    return value
  }
  const results=[]
  const metadata={started_at:new Date().toISOString(),platform:process.platform,backend,
    backend_sha256:createHash('sha256').update(await readFile(backend)).digest('hex'),
    model:configuration.model,protocol:configuration.protocol,host:new URL(configuration.base_url).hostname,
    reasoning_mode:configuration.reasoning_mode,reasoning_effort:configuration.reasoning_effort,
    context_window:configuration.context_window,max_output_tokens:configuration.max_output_tokens,
    runtime_manifest:options.runtime_manifest,temporary,
    limits:{seconds_per_case:options.seconds_per_case??240,max_steps_per_agent:24,max_children:2,observed_tokens_per_case:180000},
    notes:['Production API and model adapter; no mock model in this suite.',
      'Fresh workspace and conversation for every case; same input bytes and saved model settings.',
      'OS-sandboxed commands with network denied are approved once by the test driver; this is not human approval latency.',
      'Usage includes recorded main/child model responses; missing, failed, retry, Wiki and summary calls may not be included.',
      'No personal MCP/browser connection is copied into this accounting task. Permission/MCP behavior has a separate deterministic suite.',
      'Public transcripts exclude reasoning; internal persisted provider state stays in the temporary API data directory.']}
  let current
  try {
    origin=await Promise.race([ready,delay(30_000).then(()=>{throw new Error('Backend readiness timeout')})])
    const model=await api('/models',configuration)
    metadata.registered_model=model
    await json(join(output,'metadata.json'), metadata)
    const order=options.order??['single','auto','delegated','delegated','single','auto','auto','delegated','single']
    const repeats={}
    for(const mode of order) {
      const repeat=repeats[mode]=(repeats[mode]??0)+1
      const name=`${mode}-${repeat}`
      const workspace=join(temporary,name)
      await createFixture(workspace)
      const prompt=promptFor(mode)
      const artifact=join(output,name)
      await mkdir(artifact)
      await writeFile(join(artifact,'prompt.txt'),prompt)
      const started=Date.now()
      const root=await api('/runs',{prompt,model_id:model.id,workspace_path:workspace,
        permission_mode:'workspace_write',execution_policy:{max_steps:24,max_agents:2}})
      current=root.id
      console.log(JSON.stringify({event:'case_started',name,run_id:root.id}))
      const approvals=[]
      const processed=new Set()
      let stoppedReason=null,details=[],stable=0,lastProgress=0,lastSignature=''
      for(;;) {
        const listed=await api('/runs')
        const members=listed.filter((run)=>run.id===root.id || run.delegation?.root_run_id===root.id)
        details=await Promise.all(members.map((run)=>api(`/runs/${run.id}`)))
        const review=await api(`/runs/${root.id}/workspace`)
        for(const approval of review.approvals) {
          if(processed.has(approval.id)) continue
          processed.add(approval.id)
          const cwd=relative(workspace,approval.cwd)
          const inside=cwd==='' || (!cwd.startsWith('..') && !isAbsolute(cwd))
          const allow=inside && review.sandbox_available && approval.network_access==='denied'
            && approval.execution_mode === 'sandboxed'
          approvals.push({...approval,allow,scope:'once',approved_by:'evaluation_driver',time:new Date().toISOString()})
          await api(`/runs/${root.id}/approvals/${approval.id}`,{allow,scope:'once'})
          console.log(JSON.stringify({event:'command_approval',name,allow,execution_mode:approval.execution_mode}))
        }
        const tokens=details.reduce((sum,run)=>sum+(run.context?.observed_usage?.input_tokens??0)+(run.context?.observed_usage?.output_tokens??0),0)
        const signature=details.map(run=>`${run.id}:${run.status}:${run.revision}`).join('|')
        if(signature!==lastSignature) {stable=0;lastSignature=signature} else stable++
        if(!details.some(active) && !review.busy && stable>=2) break
        if(Date.now()-started>metadata.limits.seconds_per_case*1000 || tokens>metadata.limits.observed_tokens_per_case) {
          stoppedReason=tokens>metadata.limits.observed_tokens_per_case?'observed_token_budget':'timeout'
          await api(`/runs/${root.id}/cancel`,{})
          for(let index=0;index<30;index++) {
            await delay(200)
            details=await Promise.all(members.map((run)=>api(`/runs/${run.id}`)))
            if(!details.some(active)) break
          }
          break
        }
        if(Date.now()-lastProgress>20000) {
          lastProgress=Date.now()
          console.log(JSON.stringify({event:'case_progress',name,elapsed_seconds:Math.round((Date.now()-started)/1000),members:details.map(run=>({id:run.id,status:run.status,steps:eventCounts(run,'model_request_started')})),observed_tokens:tokens}))
        }
        await delay(500)
      }
      const duration_ms=Date.now()-started
      const grading=await score(workspace)
      const children=details.filter(run=>run.id!==root.id)
      const coordinationPassed=mode==='single'?children.length===0:mode==='delegated'?children.length===2:true
      const statusesPassed=details.every(run=>['completed','waiting_for_input'].includes(run.status))
      const result={name,mode,repeat,run_id:root.id,duration_ms,stopped_reason:stoppedReason,
        passed:grading.passed&&coordinationPassed&&statusesPassed&&!stoppedReason,
        grading,coordination_passed:coordinationPassed,statuses_passed:statusesPassed,
        child_count:children.length,approved_commands:approvals.filter(a=>a.allow).length,
        denied_commands:approvals.filter(a=>!a.allow).length,
        observed_usage:details.reduce((sum,run)=>{const usage=run.context?.observed_usage;return {
          input_tokens:sum.input_tokens+(usage?.input_tokens??0),output_tokens:sum.output_tokens+(usage?.output_tokens??0),responses:sum.responses+(usage?.responses??0)}},{input_tokens:0,output_tokens:0,responses:0}),
        members:details.map(run=>({id:run.id,task:run.title,status:run.status,error:run.error,
          delegation:run.delegation,usage:run.context?.observed_usage,steps:eventCounts(run,'model_request_started'),tool_failures:eventCounts(run,'tool_call_failed')}))}
      await json(join(artifact,'result.json'),result)
      await json(join(artifact,'approvals.json'),approvals)
      await json(join(artifact,'transcripts.json'),details.map(publicRun))
      await cp(workspace,join(artifact,'workspace'),{recursive:true})
      results.push(result)
      await json(join(output,'results.json'),results)
      console.log(JSON.stringify({event:'case_finished',name,passed:result.passed,checks:`${grading.passedChecks}/${grading.totalChecks}`,children:children.length,seconds:Math.round(duration_ms/1000),usage:result.observed_usage}))
      current=null
    }
  } finally {
    if(current&&origin) await api(`/runs/${current}/cancel`,{}).catch(()=>{})
    child.stdin.end()
    await Promise.race([exited,delay(10000)])
    if(child.exitCode===null && child.signalCode===null) child.kill('SIGTERM')
    await json(join(output,'results.json'),results)
  }
  console.log(JSON.stringify({event:'suite_finished',output,cases:results.length,passed:results.filter(r=>r.passed).length}))
  return results
}
