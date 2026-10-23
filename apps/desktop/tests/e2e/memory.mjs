import assert from 'node:assert/strict'
import { mkdtemp, mkdir } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-memory-e2e-'))
const dataDirectory = join(directory, 'data')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const ordinaryRequests = []
const compilationRequests = []
const errors = []
const fixtureErrors = []
const overview = '目标：整理本地资料，产出 **Markdown 报告**。'
const update = {
  overview,
  tasks: [
    {
      id: 'publishing',
      kind: 'constraint',
      status: 'active',
      text: '禁止公网发布',
      evidence: [{ sequence: 0, quote: '禁止公网发布' }],
    },
  ],
  pages: [
    {
      id: 'goals',
      title: '目标与约束',
      content: '- 只使用本地资料\n- 禁止公网发布\n- 交付 Markdown 报告',
      source_sequences: [0],
    },
  ],
}
const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      return response.end(JSON.stringify({ data: [] }))
    }
    assert.equal(request.method, 'POST')
    assert.equal(request.url, '/v1/chat/completions')
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const compiling = body.messages.some(
      (message) =>
        message.role === 'system' && message.content.includes('OPENHARNESS_SESSION_WIKI_V1'),
    )
    const content = compiling
      ? JSON.stringify(update)
      : `## 本地资料整理\n\n已记录约束，最终交付 Markdown 报告。\n\n${'逐项核对本地资料，保存来源与处理进度，不上传任何文档。\n\n'.repeat(180)}`
    if (compiling) compilationRequests.push(body)
    else ordinaryRequests.push(body)
    const usage = { prompt_tokens: 700, completion_tokens: compiling ? 120 : 2000 }
    if (body.stream) {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' })
      response.write(
        `data: ${JSON.stringify({
          id: 'memory-fixture',
          choices: [{ index: 0, delta: { role: 'assistant', content }, finish_reason: 'stop' }],
        })}\n\n`,
      )
      response.write(`data: ${JSON.stringify({ choices: [], usage })}\n\n`)
      return response.end('data: [DONE]\n\n')
    }
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(
      JSON.stringify({
        id: 'memory-fixture',
        choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }],
        usage,
      }),
    )
  } catch (error) {
    fixtureErrors.push(error.message)
    response.writeHead(500).end('Memory fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  api_key: 'local-memory-fixture-key',
  model: 'memory-fixture',
  context_window: 64000,
  max_output_tokens: 8192,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: dataDirectory }
delete env.ELECTRON_RUN_AS_NODE
let application
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
async function settings(page) {
  return page.evaluate(() => window.harness.extensionSettings())
}
async function memory(page, runId) {
  return page.evaluate((id) => window.harness.runMemory(id), runId)
}
async function openRun(page, runId) {
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, runId)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeVisible()
  const toggle = page.getByRole('button', { name: '打开运行详情', exact: true })
  if (await toggle.isVisible()) await toggle.click()
  await page.getByRole('tab', { name: '记忆', exact: true }).click()
}
async function waitIdle(page, runId) {
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, runId), {
      timeout: 30_000,
    })
    .toBe('waiting_for_input')
}

try {
  let page = await launch()
  await page.getByRole('link', { name: '插件', exact: true }).click()
  let card = page.getByRole('article', { name: '插件 LLM Wiki', exact: true })
  await expect(card).toBeVisible()
  await expect(card.getByRole('switch', { name: '启用插件 LLM Wiki' })).toBeChecked()
  await expect(card.getByRole('button', { name: /卸载|更新/ })).toHaveCount(0)
  assert.equal((await settings(page)).config.llm_wiki_enabled, true)
  await page.screenshot({ path: join(artifacts, 'memory-plugin-settings.png') })

  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    configuration,
  )
  const created = await page.evaluate(
    (modelId) =>
      window.harness.createRun({
        prompt: '为当前会话保留约束：只使用本地资料，禁止公网发布。最终交付 Markdown 报告。',
        model_id: modelId,
      }),
    configured.model.id,
  )
  const runId = created.id
  await waitIdle(page, runId)
  await expect
    .poll(async () => (await memory(page, runId)).status, { timeout: 30_000 })
    .toBe('ready')
  const saved = await memory(page, runId)
  assert.equal(saved.overview, overview)
  assert.deepEqual(saved.pages[0].source_sequences, [0])
  assert.equal(saved.tasks[0].status, 'active')
  assert.equal(saved.pending, false, 'terminal bookkeeping does not create an ingestion backlog')
  assert(compilationRequests.length > 0)
  await openRun(page, runId)
  await expect(page.getByRole('region', { name: '记忆概览' })).toContainText('Markdown 报告')
  await expect(page.getByRole('region', { name: '任务记录' })).toContainText('当前有效')
  await page.getByRole('region', { name: '任务记录' }).locator('summary').click()
  await expect(page.getByRole('region', { name: '任务记录' })).toContainText('禁止公网发布')
  await page.locator('.memory-pages .memory-page > summary').click()
  await expect(page.locator('.memory-pages .memory-page')).toContainText('禁止公网发布')
  await page.getByRole('button', { name: '#0', exact: true }).click()
  await expect(page.getByRole('tab', { name: /时间线/ })).toHaveAttribute('aria-selected', 'true')
  await expect(page.locator('#timeline-event-0')).toBeFocused()
  await expect(page.locator('#timeline-event-0 details')).toHaveAttribute('open', '')
  await page.getByRole('tab', { name: '记忆', exact: true }).click()
  await page.getByRole('region', { name: '任务记录' }).locator('summary').click()
  await page.locator('.memory-pages .memory-page > summary').click()
  await page.screenshot({ path: join(artifacts, 'memory-inspector.png') })
  const beforeView = compilationRequests.length
  await page.getByRole('button', { name: '刷新会话记忆' }).click()
  await expect(page.getByRole('button', { name: '刷新会话记忆' })).toBeEnabled()
  assert.equal(compilationRequests.length, beforeView, 'reading a wiki does not compile it')

  await page.getByRole('link', { name: '管理记忆插件' }).click()
  await card.getByRole('switch', { name: '启用插件 LLM Wiki' }).click()
  await expect(card.getByRole('switch', { name: '启用插件 LLM Wiki' })).not.toBeChecked()
  // Saving unrelated settings through the main-process validator must keep it off.
  const persisted = await page.evaluate(async () => {
    const current = await window.harness.extensionSettings()
    return window.harness.saveExtensions({
      ...current.config,
      disabled_skill_ids: ['fixture-skill'],
    })
  })
  assert.equal(persisted.config.llm_wiki_enabled, false)
  await page.evaluate((id) => window.harness.submitInput(id, '继续检查文档。'), runId)
  await waitIdle(page, runId)
  assert.equal(compilationRequests.length, beforeView, 'disabled memory must not compile new turns')
  const disabledRequest = ordinaryRequests.at(-1)
  assert(!(disabledRequest.tools ?? []).some((tool) => tool.function.name === 'session_memory'))
  assert(
    !JSON.stringify(disabledRequest.messages).includes('Session memory: derived historical notes'),
  )
  assert.deepEqual((await memory(page, runId)).pages, saved.pages)
  await application.close()
  application = undefined

  page = await launch()
  await page.getByRole('link', { name: '插件', exact: true }).click()
  card = page.getByRole('article', { name: '插件 LLM Wiki', exact: true })
  await expect(card.getByRole('switch', { name: '启用插件 LLM Wiki' })).not.toBeChecked()
  assert.equal((await settings(page)).config.llm_wiki_enabled, false)
  const restored = await memory(page, runId)
  assert.equal(restored.status, 'disabled')
  assert.deepEqual(restored.pages, saved.pages)
  assert.deepEqual(restored.tasks, saved.tasks)
  await openRun(page, runId)
  await expect(page.locator('.memory-inspector')).toContainText('已关闭')
  await expect(page.getByRole('region', { name: '记忆概览' })).toContainText('Markdown 报告')
  await page.screenshot({ path: join(artifacts, 'memory-disabled-restored.png') })
  assert.equal(compilationRequests.length, beforeView, 'restart and viewing must be read-only')

  await page.getByRole('link', { name: '管理记忆插件' }).click()
  await card.getByRole('switch', { name: '启用插件 LLM Wiki' }).click()
  await expect(card.getByRole('switch', { name: '启用插件 LLM Wiki' })).toBeChecked()
  // Reconfigure the local fixture for hosts without persistent credential storage.
  await page.evaluate((input) => window.harness.configureModel(input), configuration)
  await page.evaluate((id) => window.harness.submitInput(id, '继续整理，复用之前的约束。'), runId)
  await waitIdle(page, runId)
  await expect
    .poll(async () => (await memory(page, runId)).revision, { timeout: 30_000 })
    .toBeGreaterThan(saved.revision)
  assert(ordinaryRequests.at(-1).tools.some((tool) => tool.function.name === 'session_memory'))
  assert(
    JSON.stringify(ordinaryRequests.at(-1).messages).includes(
      'Session memory: derived historical notes',
    ),
  )
  assert.deepEqual(errors, [])
  assert.deepEqual(fixtureErrors, [])
  console.log(
    JSON.stringify(
      {
        passed: true,
        directory,
        ordinaryCalls: ordinaryRequests.length,
        compilationCalls: compilationRequests.length,
        checks: [
          'default enabled',
          'automatic wiki compilation',
          'read-only inspector',
          'source timeline navigation',
          'disable revokes model access',
          'disabled config survives unrelated saves and restart',
          'saved pages retained while disabled',
          're-enable updates existing memory',
        ],
      },
      null,
      2,
    ),
  )
} finally {
  await application?.close()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
