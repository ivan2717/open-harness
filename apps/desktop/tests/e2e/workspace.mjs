import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, writeFile, realpath, access } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { setTimeout as delay } from 'node:timers/promises'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-workspace-e2e-'))
const project = join(directory, 'project')
await mkdir(project)
const root = await realpath(project)
const original = 'User changes already here.\n'
await writeFile(join(project, 'note.txt'), original)
await writeFile(join(project, 'AGENTS.md'), 'Keep all pre-existing user changes.\n')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const calls = []
const counts = new Map()
let toolSequence = 0
const server = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.writeHead(404, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ error: 'This fixture exposes no model metadata.' }))
    return
  }
  assert.equal(request.method, 'POST')
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  const body = JSON.parse(Buffer.concat(chunks).toString())
  calls.push(body)
  const user = body.messages.filter((message) => message.role === 'user').at(-1)?.content ?? ''
  const count = counts.get(user) ?? 0
  counts.set(user, count + 1)
  if (user === 'BLOCK_MODEL') return
  let call
  if (user === 'E2E_LOCAL_WORK') {
    if (count === 0) call = ['read_file', { path: 'note.txt' }]
    if (count === 1)
      call = [
        'apply_patch',
        {
          patch:
            '*** Begin Patch\n*** Update File: note.txt\n@@\n User changes already here.\n+Agent addition.\n*** End Patch',
        },
      ]
    if (count === 2) call = ['run_command', { command: "printf 'LOCAL_OK\\n'", timeout_seconds: 5 }]
  } else if (user === 'READ_ONLY_WORK' && count === 0) {
    call = [
      'apply_patch',
      { patch: '*** Begin Patch\n*** Add File: blocked.txt\n+must not exist\n*** End Patch' },
    ]
  } else if (user === 'DENY_COMMAND' && count === 0) {
    call = ['run_command', { command: 'printf denied > denied.txt', timeout_seconds: 5 }]
  } else if (user === 'SLEEP_BEFORE_CLOSE' && count === 0) {
    call = [
      'run_command',
      {
        command:
          'printf started > close-started.txt; (sleep 4; printf leaked > close-leaked.txt) & wait',
        timeout_seconds: 20,
      },
    ]
  }
  response.writeHead(200, { 'Content-Type': 'text/event-stream' })
  const send = (data) => response.write(`data: ${JSON.stringify(data)}\n\n`)
  send({
    id: `response-${++toolSequence}`,
    choices: [
      {
        index: 0,
        delta: call
          ? {
              role: 'assistant',
              tool_calls: [
                {
                  index: 0,
                  id: `tool-${toolSequence}`,
                  type: 'function',
                  function: { name: call[0], arguments: JSON.stringify(call[1]) },
                },
              ],
            }
          : { role: 'assistant', content: '本地检查已完成。保留已有内容，可继续此会话。' },
        finish_reason: call ? 'tool_calls' : 'stop',
      },
    ],
  })
  send({ choices: [], usage: { prompt_tokens: 1000, completion_tokens: 100 } })
  response.write('data: [DONE]\n\n')
  response.end()
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const modelInput = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  api_key: 'local-workspace-fixture-key',
  model: 'workspace-fixture',
  context_window: 32000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: join(directory, 'data') }
delete env.ELECTRON_RUN_AS_NODE
let application
const errors = []
async function launch() {
  application = await _electron.launch({
    executablePath: require('electron'),
    args: [resolve('.')],
    env,
    timeout: 30_000,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  return page
}
async function settled(page, id) {
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).status, {
      timeout: 30_000,
    })
    .toBe('waiting_for_input')
  return page.evaluate((id) => window.harness.getRun(id), id)
}
async function navigate(page, id) {
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
}
async function crashOwnedBackend() {
  // This deliberately crashes only this test app's direct API child. Production
  // recovery never kills a saved PID, which could have been reused after a crash.
  const { stdout } = await promisify(execFile)('ps', ['-axo', 'pid=,ppid=,comm='])
  const parent = application.process().pid
  const children = stdout
    .split('\n')
    .map((line) => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
    .filter((match) => match && Number(match[2]) === parent && /\/api$/.test(match[3]))
  assert.equal(children.length, 1, 'test must identify exactly one owned API child')
  process.kill(Number(children[0][1]), 'SIGKILL')
}
try {
  let page = await launch()
  // This fixture asserts the local workflow and coordination tools; memory has its own E2E suite.
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  await expect(page.getByLabel('选择模型')).toHaveCount(0)
  await expect(page.getByRole('link', { name: '添加模型', exact: true })).toBeVisible()
  await expect(page.getByText(/Local demo/)).toHaveCount(0)
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  // Calling the limited bridge directly must not bypass native directory selection.
  await assert.rejects(
    page.evaluate(
      (root) =>
        window.harness.createRun({
          prompt: 'Unauthorized',
          model_id: 'local-demo',
          workspace_path: root,
        }),
      root,
    ),
    /选择项目/,
  )
  const legacy = await page.evaluate(() =>
    window.harness.createRun({
      prompt: 'Existing conversation without a project',
      model_id: 'local-demo',
    }),
  )
  await page.evaluate((id) => window.harness.cancelRun(id), legacy.id)
  await navigate(page, legacy.id)
  await expect(page.getByRole('button', { name: '更多操作', exact: true })).toHaveCount(0)
  await page.getByRole('link', { name: /新建会话/ }).click()
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await application.evaluate(({ dialog }, root) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [root] })
  }, root)
  await page.getByRole('button', { name: '选择项目', exact: true }).click()
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toContainText('只读')
  await page.getByRole('button', { name: '项目权限', exact: true }).click()
  await expect(page.getByRole('menuitemradio', { name: /只读/ })).toHaveAttribute(
    'aria-checked',
    'true',
  )
  await page.getByRole('menuitemradio', { name: /允许修改/ }).click()
  await page.getByRole('textbox', { name: '任务描述', exact: true }).fill('E2E_LOCAL_WORK')
  await page.getByRole('button', { name: '项目', exact: true }).click()
  await page.getByRole('menuitem', { name: '移除项目', exact: true }).click()
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue('')
  await page.getByRole('button', { name: '选择项目', exact: true }).click()
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toContainText('只读')
  await page.getByRole('button', { name: '项目权限', exact: true }).click()
  await expect(page.getByRole('menuitemradio', { name: /只读/ })).toHaveAttribute(
    'aria-checked',
    'true',
  )
  await page.getByRole('menuitemradio', { name: /允许修改/ }).click()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeEditable()
  await expect(page.getByRole('link', { name: '添加模型', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()
  await page.getByRole('textbox', { name: '任务描述', exact: true }).press('Enter')
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    'E2E_LOCAL_WORK',
  )
  await page.screenshot({ path: join(artifacts, 'desktop-workspace-entry-no-model.png') })

  await page.getByRole('link', { name: '添加模型', exact: true }).click()
  await expect(page.getByRole('heading', { name: '接入模型', exact: true })).toBeVisible()
  await page.getByRole('link', { name: '会话', exact: true }).click()
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toContainText(
    '允许修改',
  )
  await expect(
    page.locator('.composer-context-bar').getByRole('button', { name: '项目', exact: true }),
  ).toHaveAttribute('title', root)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    'E2E_LOCAL_WORK',
  )
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()

  await page.getByRole('link', { name: '添加模型', exact: true }).click()
  await page.getByLabel('API 基础地址').fill(modelInput.base_url)
  await page.getByLabel('模型名称', { exact: true }).fill(modelInput.model)
  await page.getByLabel('API key', { exact: true }).fill(modelInput.api_key)
  await page.locator('.model-advanced > summary').click()
  await page.getByLabel('上下文窗口（tokens）').fill(String(modelInput.context_window))
  await page.getByLabel('单次输出上限（tokens）').fill(String(modelInput.max_output_tokens))
  await page.getByRole('button', { name: '保存并使用', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('模型已接入')
  const configured = {
    model: (await page.evaluate(() => window.harness.config())).models.find(
      (model) => model.name === modelInput.model,
    ),
  }
  assert(configured.model, 'the configured fixture must appear in the model picker')
  await page.getByRole('link', { name: '会话', exact: true }).click()
  await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
    configured.model.name,
  )
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toContainText(
    '允许修改',
  )
  await expect(
    page.locator('.composer-context-bar').getByRole('button', { name: '项目', exact: true }),
  ).toHaveAttribute('title', root)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    'E2E_LOCAL_WORK',
  )
  await page.getByRole('button', { name: '选择模型', exact: true }).click()
  await page.getByRole('menuitem', { name: /添加.*管理模型/ }).click()
  await expect(page.locator('.settings-model').filter({ hasText: /Local demo/ })).toHaveCount(0)
  await page.getByRole('link', { name: '设置', exact: true }).click()
  await page.getByText('开发调试', { exact: true }).click()
  await page.getByRole('button', { name: '运行上下文演示', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain('/runs/')
  const demoId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  assert.equal(
    (await page.evaluate((id) => window.harness.getRun(id), demoId)).model_id,
    'local-demo',
  )
  await page.evaluate((id) => window.harness.cancelRun(id), demoId)
  await page.getByRole('link', { name: /新建会话/ }).click()
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue('')
  await page.getByRole('button', { name: '在 project 中新建会话', exact: true }).click()
  await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
    configured.model.name,
  )
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toContainText('只读')
  await page.getByRole('button', { name: '项目权限', exact: true }).click()
  await page.getByRole('menuitemradio', { name: /允许修改/ }).click()
  await expect(
    page.locator('.composer-context-bar').getByRole('button', { name: '项目', exact: true }),
  ).toHaveAttribute('title', root)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    'E2E_LOCAL_WORK',
  )
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeEnabled()
  await page.screenshot({ path: join(artifacts, 'desktop-workspace-entry-ready.png') })
  await page
    .locator('.composer-stack')
    .screenshot({ path: join(artifacts, 'desktop-composer-reference.png') })
  const projectPicker = page.getByRole('button', { name: '项目', exact: true })
  const permission = page.getByRole('button', { name: '项目权限', exact: true })
  const modelPicker = page.getByRole('button', { name: '选择模型', exact: true })
  await projectPicker.focus()
  await projectPicker.press('Enter')
  const changeProject = page.getByRole('menuitem', { name: '更换项目', exact: true })
  await expect(changeProject).toBeFocused()
  await changeProject.press('ArrowDown')
  await expect(page.getByRole('menuitem', { name: '移除项目', exact: true })).toBeFocused()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('menu')).toHaveCount(0)
  await expect(projectPicker).toBeFocused()
  await projectPicker.click()
  await page.screenshot({ path: join(artifacts, 'desktop-composer-project-menu.png') })
  await permission.click()
  await expect(projectPicker).toHaveAttribute('aria-expanded', 'false')
  await expect(page.getByRole('menu')).toHaveCount(1)
  await expect(page.getByRole('menuitemradio', { name: /允许修改/ })).toHaveAttribute(
    'aria-checked',
    'true',
  )
  await page.screenshot({ path: join(artifacts, 'desktop-composer-permission-menu.png') })
  await page.keyboard.press('Escape')
  await expect(permission).toBeFocused()
  await modelPicker.click()
  await expect(
    page.getByRole('menuitemradio', { name: configured.model.name, exact: true }),
  ).toHaveAttribute('aria-checked', 'true')
  await expect(page.getByRole('menuitemradio', { name: /Local demo/ })).toHaveCount(0)
  await expect(page.getByRole('menuitem', { name: /添加.*管理模型/ })).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'desktop-composer-model-menu.png') })
  await page.keyboard.press('Escape')
  await expect(modelPicker).toBeFocused()
  await projectPicker.click()
  await page.getByRole('textbox', { name: '任务描述', exact: true }).click()
  await expect(page.getByRole('menu')).toHaveCount(0)
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 680),
  )
  await expect.poll(() => page.evaluate(() => window.innerWidth)).toBe(1000)
  for (const control of [projectPicker, permission, modelPicker])
    await expect(control).toBeInViewport()
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeInViewport()
  await permission.click()
  await expect(page.getByRole('menuitemradio', { name: /只读/ })).toBeInViewport()
  await expect(page.getByRole('menuitemradio', { name: /允许修改/ })).toBeInViewport()
  await page.screenshot({ path: join(artifacts, 'desktop-composer-compact.png') })
  await page.keyboard.press('Escape')
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth),
    false,
  )
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    'E2E_LOCAL_WORK',
  )
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1440, 940),
  )
  await expect.poll(() => page.evaluate(() => window.innerWidth)).toBe(1440)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain('/runs/')
  const id = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await expect(page.getByRole('button', { name: '允许本次', exact: true })).toBeVisible({
    timeout: 20_000,
  })
  assert.equal(await readFile(join(project, 'note.txt'), 'utf8'), `${original}Agent addition.\n`)
  await page.screenshot({ path: join(artifacts, 'desktop-workspace-approval.png') })
  await page.getByRole('button', { name: '允许本次', exact: true }).click()
  await settled(page, id)
  const workRequests = calls.filter(
    (body) =>
      body.messages.filter((message) => message.role === 'user').at(-1)?.content ===
      'E2E_LOCAL_WORK',
  )
  assert.deepEqual(
    workRequests[0].tools.map((tool) => tool.function.name).sort(),
    [
      'agent_control',
      'apply_patch',
      'list_files',
      'load_workspace_dependencies',
      'manage_command',
      'read_file',
      'run_command',
      'search_files',
      'spawn_agent',
    ],
    'only available local workflow and coordination tools are sent to the provider',
  )
  for (const request of workRequests) {
    assert.deepEqual(request.tools, workRequests[0].tools, 'one turn uses one tool snapshot')
  }
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
    configured.model.name,
  )
  await expect(page.getByRole('button', { name: '项目', exact: true })).toHaveCount(0)
  const review = await page.evaluate((id) => window.harness.getWorkspace(id), id)
  assert.equal(review.root, root)
  assert.equal(review.permission_mode, 'workspace_write')
  assert(
    review.changes.some(
      (change) => change.path === 'note.txt' && change.before === original && change.revertible,
    ),
  )
  assert(
    calls.some((body) =>
      JSON.stringify(body.messages).includes('Keep all pre-existing user changes.'),
    ),
  )
  await page.getByRole('button', { name: '查看项目结果', exact: true }).click()
  await page.locator('.file-change summary').filter({ hasText: 'note.txt' }).click()
  await expect(page.locator('.file-before')).toContainText(original.trim())
  await expect(page.locator('.file-after')).toContainText('Agent addition.')
  await page.screenshot({ path: join(artifacts, 'desktop-workspace-changes.png') })
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 680),
  )
  await expect(page.getByRole('button', { name: '关闭项目结果' })).toBeInViewport()
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth),
    false,
  )
  await page.getByRole('button', { name: '撤销此文件', exact: true }).click()
  await page.getByRole('button', { name: '确认撤销', exact: true }).click()
  await expect.poll(() => readFile(join(project, 'note.txt'), 'utf8')).toBe(original)
  await expect(page.getByText('未检测到可展示的文本改动。', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: '关闭项目结果' }).click()

  await page.evaluate((id) => window.harness.submitInput(id, 'DENY_COMMAND'), id)
  await expect(page.getByRole('button', { name: '拒绝', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '拒绝', exact: true }).click()
  await settled(page, id)
  await assert.rejects(access(join(project, 'denied.txt')))

  const readOnly = await page.evaluate(
    ({ root, model }) =>
      window.harness.createRun({ prompt: 'READ_ONLY_WORK', model_id: model, workspace_path: root }),
    { root, model: configured.model.id },
  )
  const readOnlyResult = await settled(page, readOnly.id)
  assert(readOnlyResult.messages.some((message) => message.role === 'tool' && message.is_error))
  await assert.rejects(access(join(project, 'blocked.txt')))

  const blocked = await page.evaluate(
    ({ root, model }) =>
      window.harness.createRun({ prompt: 'BLOCK_MODEL', model_id: model, workspace_path: root }),
    { root, model: configured.model.id },
  )
  await expect
    .poll(() =>
      calls.some((body) => body.messages.some((message) => message.content === 'BLOCK_MODEL')),
    )
    .toBe(true)
  await crashOwnedBackend()
  await application.close()
  application = undefined
  page = await launch()
  const restoredConfig = await page.evaluate(() => window.harness.config())
  if (!restoredConfig.models.find((model) => model.id === configured.model.id)?.available) {
    await page.evaluate((input) => window.harness.configureModel(input), modelInput)
    await page.reload()
  }
  const interrupted = await page.evaluate((id) => window.harness.getRun(id), blocked.id)
  assert.equal(interrupted.status, 'interrupted')
  assert.equal(interrupted.workspace_path, root)
  await navigate(page, blocked.id)
  await expect(page.getByRole('button', { name: '继续任务', exact: true })).toBeVisible()
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('继续恢复，先检查已有结果。')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await settled(page, blocked.id)
  assert.equal(await page.evaluate(() => window.location.hash.split('/runs/')[1]), blocked.id)
  assert.equal(await readFile(join(project, 'note.txt'), 'utf8'), original)
  await navigate(page, id)
  const afterRestart = await page.evaluate((id) => window.harness.getWorkspace(id), id)
  assert(afterRestart.commands.some((command) => command.id === review.commands.at(-1).id))
  assert.equal(afterRestart.approvals.length, 0)
  await page.evaluate((id) => window.harness.submitInput(id, 'SLEEP_BEFORE_CLOSE'), id)
  await expect(page.getByRole('button', { name: '允许本次', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '允许本次', exact: true }).click()
  await expect
    .poll(async () => readFile(join(project, 'close-started.txt'), 'utf8').catch(() => ''), {
      intervals: [100],
      timeout: 5_000,
    })
    .toBe('started')
  const shutdownStarted = Date.now()
  await application.close()
  application = undefined
  await delay(Math.max(0, 4_500 - (Date.now() - shutdownStarted)))
  await assert.rejects(
    access(join(project, 'close-leaked.txt')),
    'normal quit must stop shell descendants before their delayed write',
  )
  page = await launch()
  const stopped = await page.evaluate((id) => window.harness.getRun(id), id)
  assert.equal(stopped.status, 'cancelled', 'normal quit durably cancels active work')
  const stoppedWorkspace = await page.evaluate((id) => window.harness.getWorkspace(id), id)
  assert.equal(stoppedWorkspace.commands.at(-1).status, 'cancelled')
  assert.equal(stoppedWorkspace.approvals.length, 0)

  const finalConfig = await page.evaluate(() => window.harness.config())
  if (!finalConfig.models.find((model) => model.id === configured.model.id)?.available) {
    await page.evaluate((input) => window.harness.configureModel(input), modelInput)
    await page.reload()
  }
  await application.evaluate(({ dialog }, root) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [root] })
  }, root)
  for (const editDraft of [false, true]) {
    const prompt = editDraft ? 'DELAYED_CREATE_EDITED' : 'DELAYED_CREATE_UNCHANGED'
    const nextDraft = 'Keep this new draft while the previous creation finishes.'
    await page.getByRole('button', { name: '选择项目', exact: true }).click()
    await page.getByRole('textbox', { name: '任务描述', exact: true }).fill(prompt)
    await application.evaluate((_electron, prompt) => {
      // Delay delivery of one real Rust response, preserving the trusted IPC,
      // native directory grant and provider request instead of mocking createRun.
      const originalFetch = globalThis.fetch
      const gate = { calls: 0, ready: false, run: null, release: undefined, originalFetch }
      const wait = new Promise((resolve) => (gate.release = resolve))
      globalThis.__openharnessDelayedCreateTest = gate
      globalThis.fetch = async (...args) => {
        const [url, input] = args
        const matches =
          input?.method === 'POST' &&
          new URL(String(url)).pathname === '/api/runs' &&
          JSON.parse(input.body).prompt === prompt
        const response = await originalFetch(...args)
        if (matches) {
          gate.calls += 1
          gate.run = await response.clone().json()
          gate.ready = true
          await wait
        }
        return response
      }
    }, prompt)
    await page.getByRole('button', { name: '开始运行', exact: true }).click()
    await expect
      .poll(() => application.evaluate(() => globalThis.__openharnessDelayedCreateTest.ready))
      .toBe(true)
    await page.getByRole('link', { name: '设置', exact: true }).click()
    await expect(page.getByRole('heading', { name: '界面偏好', exact: true })).toBeVisible()
    await expect(page.locator('.developer-tools button')).toBeDisabled()
    await page.getByRole('link', { name: '会话', exact: true }).click()
    for (const name of ['开始运行', '项目', '项目权限', '选择模型']) {
      await expect(page.getByRole('button', { name, exact: true })).toBeDisabled()
    }
    const description = page.getByRole('textbox', { name: '任务描述', exact: true })
    await expect(description).toBeEditable()
    await expect(description).toHaveValue(prompt)
    await description.press('Enter')
    if (editDraft) await description.fill(nextDraft)
    await page.getByRole('link', { name: '设置', exact: true }).click()
    await application.evaluate(() => globalThis.__openharnessDelayedCreateTest.release())
    await expect(page.locator('.developer-tools button')).toHaveText('运行上下文演示')
    await expect(page.locator('.developer-tools button')).toBeEnabled()
    await expect(page.getByRole('heading', { name: '界面偏好', exact: true })).toBeVisible()
    assert.equal(await page.evaluate(() => window.location.hash), '#/settings')
    const delayed = await application.evaluate(() => {
      const gate = globalThis.__openharnessDelayedCreateTest
      globalThis.fetch = gate.originalFetch
      delete globalThis.__openharnessDelayedCreateTest
      return { calls: gate.calls, run: gate.run }
    })
    assert.equal(delayed.calls, 1, 'navigation must not allow a duplicate creation')
    assert.equal(delayed.run.prompt, prompt)
    assert.equal(delayed.run.workspace_path, root)
    await settled(page, delayed.run.id)
    await page.getByRole('link', { name: '会话', exact: true }).click()
    await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue('')
    await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeEnabled()
    await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()
    await page.getByRole('button', { name: '在 project 中新建会话', exact: true }).click()
    await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
      editDraft ? nextDraft : '',
    )
    const send = page.getByRole('button', { name: '开始运行', exact: true })
    if (editDraft) await expect(send).toBeEnabled()
    else await expect(send).toBeDisabled()
    await page.getByRole('link', { name: /新建会话/ }).click()
  }
  assert.deepEqual(errors, [])
  assert.equal(review.commands.at(-1).exit_code, 0, JSON.stringify(review.commands))
  assert(review.commands.at(-1).output.includes('LOCAL_OK'))
  console.log(
    JSON.stringify(
      { passed: true, id, interruptedRun: blocked.id, project, directory, artifacts },
      null,
      2,
    ),
  )
} finally {
  await application?.close()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
