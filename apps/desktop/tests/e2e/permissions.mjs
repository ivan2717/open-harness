import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

// Run after `pnpm build` and `node scripts/backend.mjs debug`. This suite owns an
// isolated app profile and loopback provider; it never uses saved user API keys.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-permissions-e2e-'))
const artifacts = resolve(process.env.OPENHARNESS_E2E_ARTIFACTS ?? 'test-results/permissions')
const executable = process.env.OPENHARNESS_E2E_EXECUTABLE
const requestLogs = []
await mkdir(join(directory, 'project'))
await mkdir(artifacts, { recursive: true })
await Promise.all(
  ['failure.json', 'failed-run.json', 'failure.png', 'report.json'].map((name) =>
    rm(join(artifacts, name), { force: true }),
  ),
)
const project = await realpath(join(directory, 'project'))
await writeFile(join(project, 'note.txt'), 'Permission fixture.\n')
const providerErrors = []
const rendererErrors = []
const reviewCalls = []
const ordinaryCalls = []
const counts = new Map()
let sequence = 0
let fullAccessNetworkRequests = 0
const fullAccessOutput = join(directory, 'outside-project.txt')
const fullAccessProbe = join(project, 'full-access-probe.cjs')
const shellQuote = (value) =>
  process.platform === 'win32'
    ? `'${value.replaceAll("'", "''")}'`
    : `'${value.replaceAll("'", "'\"'\"'")}'`
const fullAccessCommand = `${process.platform === 'win32' ? '& ' : ''}${shellQuote(process.execPath)} ${shellQuote(fullAccessProbe)}`
const server = createServer(async (request, response) => {
  try {
    if (request.url === '/full-access-probe' && request.method === 'GET') {
      fullAccessNetworkRequests += 1
      response.writeHead(200, { 'Content-Type': 'text/plain' })
      response.end('LOCAL_NETWORK_OK')
      return
    }
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(JSON.stringify({ data: [] }))
      return
    }
    assert.equal(request.url, '/v1/chat/completions')
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const user = body.messages.filter((message) => message.role === 'user').at(-1)?.content ?? ''
    let evidence
    try {
      evidence = JSON.parse(user)
    } catch {
      /* ordinary user prompt */
    }
    let text
    let call
    if (evidence?.proposed_action) {
      reviewCalls.push(body)
      assert.equal(evidence.proposed_action.kind, 'command')
      assert.equal(evidence.proposed_action.network_access, 'allowed')
      assert.equal(body.tools?.length ?? 0, 0, 'reviewer must not have tools')
      text = JSON.stringify({
        decision: 'allow',
        risk: 'low',
        authorized: true,
        reason: '本地测试命令只输出固定文字；本次批准不创建可复用授权。',
      })
    } else {
      ordinaryCalls.push(body)
      const count = counts.get(user) ?? 0
      counts.set(user, count + 1)
      if (count === 0 && /^E2E_/.test(user)) {
        call = {
          name: 'run_command',
          arguments: {
            command: user === 'E2E_FULL_EXISTING' ? fullAccessCommand : `echo ${user}`,
            timeout_seconds: 5,
            ...(user.startsWith('E2E_FULL_') ? {} : { access: 'read_only' }),
            network_access: user === 'E2E_OFFLINE' ? 'denied' : 'allowed',
          },
        }
      }
      text = `${user} 已完成。`
    }
    const id = `permission-response-${++sequence}`
    const usage = { prompt_tokens: 120, completion_tokens: 30 }
    const tool_calls = call
      ? [
          {
            id: `tool-${sequence}`,
            type: 'function',
            function: { name: call.name, arguments: JSON.stringify(call.arguments) },
          },
        ]
      : undefined
    if (!body.stream) {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(
        JSON.stringify({
          id,
          choices: [
            {
              index: 0,
              message: { role: 'assistant', content: text, ...(tool_calls ? { tool_calls } : {}) },
              finish_reason: call ? 'tool_calls' : 'stop',
            },
          ],
          usage,
        }),
      )
      return
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const send = (value) => response.write(`data: ${JSON.stringify(value)}\n\n`)
    send({
      id,
      choices: [
        {
          index: 0,
          delta: {
            role: 'assistant',
            ...(tool_calls
              ? { tool_calls: tool_calls.map((tool, index) => ({ ...tool, index })) }
              : { content: text }),
          },
          finish_reason: call ? 'tool_calls' : 'stop',
        },
      ],
    })
    send({ choices: [], usage })
    response.end('data: [DONE]\n\n')
  } catch (error) {
    providerErrors.push(error.stack ?? error.message)
    response.writeHead(500)
    response.end('Permission fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
await writeFile(
  fullAccessProbe,
  `
const fs = require('node:fs/promises');
(async () => {
  const response = await fetch(${JSON.stringify(`http://127.0.0.1:${server.address().port}/full-access-probe`)});
  if (!response.ok) throw new Error('Local network probe failed');
  const text = await response.text();
  await fs.writeFile(${JSON.stringify(fullAccessOutput)}, text);
  console.log('E2E_FULL_EXISTING ' + text);
})().catch(error => { console.error(error); process.exitCode = 1; });
`,
)
const modelInput = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  api_key: 'synthetic-permissions-e2e-key',
  model: 'permissions-fixture',
  context_window: 64000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: join(directory, 'data') }
delete env.ELECTRON_RUN_AS_NODE
let app
let page
let runId
let modelId
let expectedPrompt
const steps = []
function checkpoint(step) {
  steps.push(step)
  console.log(`[permissions] ${step}`)
}
async function launch() {
  app = await _electron.launch({
    executablePath: executable ? resolve(executable) : require('electron'),
    args: executable ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  await app.evaluate(() => {
    globalThis.__permissionRequests = []
    const original = globalThis.fetch
    globalThis.fetch = (...args) => {
      const [url, input] = args
      if (input?.method && input.method !== 'GET')
        globalThis.__permissionRequests.push({
          time: Date.now(),
          method: input.method,
          path: new URL(String(url)).pathname,
        })
      return original(...args)
    }
  })
  page = await app.firstWindow()
  page.setDefaultTimeout(15_000)
  page.on('pageerror', (error) => rendererErrors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  // Keep this synthetic fixture credential in memory to avoid system keychain UI.
  await app.evaluate(({ safeStorage }) => {
    safeStorage.isEncryptionAvailable = () => false
  })
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    modelInput,
  )
  modelId = configured.model.id
  await page.reload()
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(1280, 900))
}
async function navigate(id) {
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  await expect(page.locator('.composer-permissions')).toBeVisible()
}
async function permissions(id = runId) {
  return page.evaluate((id) => window.harness.runPermissions(id), id)
}
async function review(id = runId) {
  return page.evaluate((id) => window.harness.getWorkspace(id), id)
}
async function settled(id = runId) {
  await expect
    .poll(
      async () => {
        const run = await page.evaluate((id) => window.harness.getRun(id), id)
        const latest = run.messages.filter((message) => message.role === 'user').at(-1)?.text
        return latest === expectedPrompt ? run.status : 'waiting_for_new_input'
      },
      { timeout: 30_000 },
    )
    .toBe('waiting_for_input')
  const state = await review(id)
  assert.equal(state.approvals.length, 0)
  assert.equal(state.commands.at(-1)?.exit_code, 0, JSON.stringify(state.commands.at(-1)))
  assert(
    state.commands.at(-1).output.includes(expectedPrompt),
    JSON.stringify(state.commands.at(-1)),
  )
  return state
}
async function submit(prompt) {
  expectedPrompt = prompt
  await page.getByRole('textbox', { name: '任务描述', exact: true }).fill(prompt)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
}
async function openPermissions() {
  await page.getByRole('link', { name: '设置', exact: true }).click()
  const details = page.locator('.permission-settings-details')
  if (!(await details.evaluate((element) => element.open)))
    await details.getByText('管理授权与审查记录', { exact: true }).click()
  await page.getByLabel('选择要管理授权的会话').selectOption(runId)
  await page.getByRole('button', { name: '查看授权记录', exact: true }).click()
  return page.getByRole('dialog', { name: '会话授权记录' })
}
async function closePermissions(panel) {
  await panel.getByRole('button', { name: '关闭会话授权记录' }).click()
  await navigate(runId)
}
const modeLabels = { ask: '请求批准', auto_review: '帮我批准', full_access: '完全访问权限' }
async function changeMode(mode) {
  const trigger = page.getByRole('button', { name: '审批方式', exact: true })
  await expect(trigger).toBeEnabled()
  await trigger.click()
  const menu = page.getByRole('menu', { name: '审批方式', exact: true })
  await expect(menu.getByRole('menuitemradio')).toHaveCount(3)
  await expect(menu.getByText('已批准的访问', { exact: true })).toHaveCount(0)
  await expect(menu.getByText('会话文件', { exact: true })).toHaveCount(0)
  await menu.getByRole('menuitemradio', { name: new RegExp(modeLabels[mode]) }).click()
  await expect
    .poll(async () => {
      const state = await permissions()
      return `${state.approval_mode}:${state.permission_mode}`
    })
    .toBe(`${mode}:workspace_write`)
  await expect(trigger).toContainText(modeLabels[mode])
}

try {
  checkpoint('launch isolated app and select new-chat approval mode')
  await launch()
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '请求批准',
  )
  await page.getByRole('button', { name: '审批方式', exact: true }).click()
  await page.getByRole('menuitemradio', { name: /帮我批准/ }).click()
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '帮我批准',
  )
  await app.evaluate(({ dialog }, project) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [project] })
  }, project)
  await page.getByRole('button', { name: '选择项目', exact: true }).click()
  await submit('E2E_OFFLINE')
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain('/runs/')
  runId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  const offline = await settled()
  assert(offline.commands.at(-1).output.includes('E2E_OFFLINE'))
  assert.equal(
    (await permissions()).approval_mode,
    'auto_review',
    'composer choice must reach the backend',
  )
  assert.equal(
    reviewCalls.length,
    0,
    'offline sandbox command must need neither human nor model review',
  )
  checkpoint('offline sandbox command executed without approval')

  assert.equal((await permissions()).permission_mode, 'workspace_write')
  // Seed an older restrictive conversation through the advanced API. Merely
  // opening its menu must not migrate permissions; explicitly reapplying even
  // the currently selected preset should make the working directory writable.
  await page.evaluate(
    (id) =>
      window.harness.updateRunPermissions(id, {
        approval_mode: 'ask',
        permission_mode: 'read_only',
      }),
    runId,
  )
  // The direct advanced-API fixture bypasses the renderer's mutation cache.
  // Reload instead of racing the five-second permissions polling interval.
  await page.reload()
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '请求批准',
  )
  assert.equal((await permissions()).permission_mode, 'read_only')
  await changeMode('ask')
  assert.equal((await permissions()).permission_mode, 'workspace_write')
  // The root policy can be saved before rebuilding a descendant fails. A
  // failed response must refresh authoritative state instead of leaving the
  // composer on a stale, more restrictive-looking preset.
  await app.evaluate(() => {
    const originalFetch = globalThis.fetch
    globalThis.fetch = async (...args) => {
      const [url, init] = args
      const matches =
        init?.method === 'PATCH' && new URL(String(url)).pathname.endsWith('/permissions')
      const response = await originalFetch(...args)
      if (!matches) return response
      globalThis.fetch = originalFetch
      if (!response.ok) return response
      return new Response(JSON.stringify({ error: 'Synthetic descendant rebuild failure' }), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      })
    }
  })
  await changeMode('auto_review')
  assert.equal((await permissions()).permission_mode, 'workspace_write')
  const refreshError = page.getByRole('button', { name: /权限更新失败：.*点击重试读取权限/ })
  await expect(refreshError).toBeVisible()
  await refreshError.click()
  await expect(refreshError).toHaveCount(0)
  checkpoint('failed permission update refreshes the saved policy without a stale mode label')
  checkpoint(
    'all presets permit workspace writes; reapplying Ask upgrades an old read-only chat explicitly',
  )
  for (const prompt of ['E2E_AUTO_NETWORK_ONE', 'E2E_AUTO_NETWORK_TWO']) {
    await submit(prompt)
    await settled()
    assert.equal((await review()).command_grants.length, 0, 'reviewer approval must be once-only')
  }
  await expect
    .poll(
      async () =>
        (await permissions()).reviews.filter((record) => record.status === 'approved').length,
    )
    .toBe(2)
  assert.equal(
    reviewCalls.length,
    2,
    'each network action must be independently reviewed without a session grant',
  )
  const records = (await permissions()).reviews
  assert(
    records.every(
      (record) => record.usage?.input_tokens === 120 && record.usage?.output_tokens === 30,
    ),
  )
  assert(
    records.every((record) => record.model_id === modelId && record.reason.includes('本次批准')),
  )
  checkpoint('two low-risk actions reviewed separately with persisted audit and token usage')

  await changeMode('ask')
  await submit('E2E_MANUAL_ALWAYS')
  await expect(page.getByRole('button', { name: '始终允许', exact: true })).toBeVisible()
  assert.equal(reviewCalls.length, 2, 'Ask must not invoke the model reviewer')
  await page.screenshot({ path: join(artifacts, 'command-approval.png') })
  await page.getByRole('button', { name: '始终允许', exact: true }).click()
  await settled()
  assert.equal((await permissions()).persistent_command_grants.length, 1)
  let panel = await openPermissions()
  await expect(panel.getByText('本会话及协作任务 · 重启后保留', { exact: true })).toBeVisible()
  await expect(panel.getByRole('heading', { name: '自动审查记录' })).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'conversation-permissions.png') })
  await panel.screenshot({ path: join(artifacts, 'permissions-dialog.png') })
  await closePermissions(panel)
  checkpoint('manual Always persisted with clear conversation scope')

  requestLogs.push(...(await app.evaluate(() => globalThis.__permissionRequests)))
  await app.close()
  app = undefined
  await launch()
  await navigate(runId)
  assert.equal((await permissions()).approval_mode, 'ask')
  assert.equal((await permissions()).persistent_command_grants.length, 1)
  await submit('E2E_AFTER_RESTART')
  const restored = await settled()
  assert(restored.commands.at(-1).output.includes('E2E_AFTER_RESTART'))
  assert.equal(reviewCalls.length, 2)
  checkpoint('Always restored after clean Electron/backend restart')

  // A separate conversation with the same project must not inherit this grant.
  await app.evaluate(({ dialog }, project) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [project] })
  }, project)
  await page.evaluate(() => window.harness.chooseWorkspace())
  const unrelated = await page.evaluate(
    ({ project, modelId }) =>
      window.harness.createRun({
        prompt: 'E2E_UNRELATED',
        model_id: modelId,
        workspace_path: project,
        execution_policy: { approval_mode: 'ask' },
      }),
    { project, modelId },
  )
  await expect.poll(async () => (await review(unrelated.id)).approvals.length).toBe(1)
  assert.equal((await permissions(unrelated.id)).persistent_command_grants.length, 0)
  await page.evaluate((id) => window.harness.cancelRun(id), unrelated.id)
  checkpoint('persistent command grant does not authorize an unrelated conversation')

  panel = await openPermissions()
  await panel.getByRole('button', { name: '撤销会话授权', exact: true }).click()
  await expect.poll(async () => (await permissions()).persistent_command_grants.length).toBe(0)
  assert.equal((await review()).command_grants.length, 0)
  await closePermissions(panel)
  await submit('E2E_AFTER_REVOKE')
  await expect(page.getByRole('button', { name: '始终允许', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '仅允许本次', exact: true }).click()
  await settled()
  assert.equal((await review()).command_grants.length, 0)
  assert.equal((await permissions()).persistent_command_grants.length, 0)
  checkpoint('revocation removes memory and disk grant; next operation prompts again')
  await changeMode('full_access')
  assert.equal((await permissions()).permission_mode, 'workspace_write')
  await expect(page.locator('.composer-permission-label')).toHaveCount(0)
  const reviewCountBeforeFull = reviewCalls.length
  await submit('E2E_FULL_EXISTING')
  const fullAccessState = await settled()
  assert.equal(fullAccessState.commands.at(-1).execution_mode, 'unrestricted')
  assert.equal(fullAccessNetworkRequests, 1, 'Full must really reach the local HTTP listener')
  assert.equal(
    await readFile(fullAccessOutput, 'utf8'),
    'LOCAL_NETWORK_OK',
    'Full must write outside the project in the isolated test directory',
  )
  assert.equal(
    reviewCalls.length,
    reviewCountBeforeFull,
    'full access must not invoke model review',
  )
  assert.equal((await permissions()).persistent_command_grants.length, 0)
  const fullTrigger = page.getByRole('button', { name: '审批方式', exact: true })
  await page.mouse.move(0, 0)
  await expect(fullTrigger).toHaveCSS('color', 'rgb(182, 106, 41)')
  await fullTrigger.hover()
  await expect(fullTrigger).toHaveCSS('color', 'rgb(182, 106, 41)')
  await page.mouse.move(0, 0)
  await fullTrigger.focus()
  await expect(fullTrigger).toHaveCSS('color', 'rgb(182, 106, 41)')
  await fullTrigger.press('Enter')
  const fullMenu = page.getByRole('menu', { name: '审批方式', exact: true })
  await expect(fullMenu.getByRole('menuitemradio', { name: /完全访问权限/ })).toHaveAttribute(
    'aria-checked',
    'true',
  )
  const fullOption = fullMenu.getByRole('menuitemradio', { name: /完全访问权限/ })
  await expect(fullOption).toHaveCSS('color', 'rgb(182, 106, 41)')
  await expect(fullOption.locator('small')).toHaveCSS('color', 'rgb(182, 106, 41)')
  await expect(fullTrigger).toHaveCSS('color', 'rgb(182, 106, 41)')
  await fullOption.hover()
  await expect(fullOption).toHaveCSS('color', 'rgb(182, 106, 41)')
  await expect(fullOption.locator('small')).toHaveCSS('color', 'rgb(182, 106, 41)')
  await page.mouse.move(0, 0)
  await expect(fullMenu.getByText('自动审查会产生少量模型用量。', { exact: true })).toHaveCount(0)
  await fullMenu.screenshot({ path: join(artifacts, 'permissions-menu.png') })
  await page.keyboard.press('Escape')
  checkpoint(
    'full access writes outside the project and reaches local HTTP without per-command approvals',
  )
  panel = await openPermissions()
  await expect(panel.getByText(/当前会话使用完全访问权限/)).toBeVisible()
  await expect(panel.getByRole('button', { name: '撤销会话授权', exact: true })).toHaveCount(0)
  await panel.getByRole('button', { name: '收回权限并停止任务', exact: true }).click()
  await expect.poll(async () => (await permissions()).approval_mode).toBe('ask')
  await closePermissions(panel)
  checkpoint('secondary management explicitly revokes full access back to asking')

  await page.getByRole('link', { name: /新建会话/ }).click()
  const newTrigger = page.getByRole('button', { name: '审批方式', exact: true })
  await expect(newTrigger).toContainText('请求批准')
  await newTrigger.click()
  await page.getByRole('menuitemradio', { name: /完全访问权限/ }).click()
  await submit('E2E_FULL_NEW_CHAT')
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain('/runs/')
  runId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await settled()
  assert.equal((await permissions()).approval_mode, 'full_access')
  assert.equal((await permissions()).permission_mode, 'workspace_write')
  assert.equal(reviewCalls.length, reviewCountBeforeFull)
  // Navigate directly, without the New Chat handler, to prove creation consumed
  // the transient choice instead of relying only on the sidebar reset.
  await page.evaluate(() => {
    window.location.hash = '/'
  })
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '请求批准',
  )
  await navigate(runId)
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '完全访问权限',
  )
  checkpoint('new full-access chat persists its mode while the next draft resets to asking')
  await page.getByRole('link', { name: '设置', exact: true }).click()
  await page.getByText('管理授权与审查记录', { exact: true }).click()
  await expect(page.getByRole('heading', { name: '始终允许的访问' })).toBeVisible()
  await expect(page.getByText('尚未保存长期授权。', { exact: true })).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'permission-settings.png') })
  assert.deepEqual(providerErrors, [])
  assert.deepEqual(rendererErrors, [])
  const report = {
    passed: true,
    runId,
    directory,
    artifacts,
    steps,
    reviewerRequests: reviewCalls.length,
    normalRequests: ordinaryCalls.length,
    fullAccessNetworkRequests,
  }
  await writeFile(join(artifacts, 'report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify(report, null, 2))
} catch (error) {
  await page?.screenshot({ path: join(artifacts, 'failure.png') }).catch(() => {})
  requestLogs.push(
    ...((await app?.evaluate(() => globalThis.__permissionRequests).catch(() => [])) ?? []),
  )
  if (runId)
    await writeFile(
      join(artifacts, 'failed-run.json'),
      JSON.stringify(await page.evaluate((id) => window.harness.getRun(id), runId), null, 2),
    ).catch(() => {})
  await writeFile(
    join(artifacts, 'failure.json'),
    JSON.stringify(
      {
        error: error.stack,
        runId,
        directory,
        steps,
        providerErrors,
        rendererErrors,
        requestLogs,
        reviewerRequests: reviewCalls.length,
      },
      null,
      2,
    ),
  )
  throw error
} finally {
  await app?.close()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
