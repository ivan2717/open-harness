import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-steering-e2e-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const calls = []
const responses = []
const errors = []
const server = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ data: [] }))
    return
  }
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  calls.push(JSON.parse(Buffer.concat(chunks).toString()))
  responses.push(response)
  response.writeHead(200, { 'Content-Type': 'text/event-stream' })
  response.write(
    `data: ${JSON.stringify({ id: `steering-response-${calls.length}`, choices: [{ index: 0, delta: { role: 'assistant', content: `正在检查任务，第 ${calls.length} 段回复。` }, finish_reason: null }] })}\n\n`,
  )
})
await new Promise((done) => server.listen(0, '127.0.0.1', done))
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application
try {
  application = await _electron.launch({
    executablePath: process.env.OPENHARNESS_E2E_EXECUTABLE ?? require('electron'),
    args: process.env.OPENHARNESS_E2E_EXECUTABLE ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  const run = await page.evaluate(async (base_url) => {
    await window.harness.setPluginEnabled('builtin.llm-wiki', false)
    const model = await window.harness.configureModel({
      protocol: 'chat_completions',
      base_url,
      model: 'steering-fixture',
      api_key: 'local-test-only',
      context_window: 64000,
      max_output_tokens: 2048,
      supports_images: false,
      reasoning_mode: 'off',
      reasoning_effort: 'medium',
    })
    return window.harness.createRun({ prompt: '帮我检查这个任务', model_id: model.model.id })
  }, `http://127.0.0.1:${server.address().port}/v1`)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, run.id)
  await page.reload()
  const editor = page.getByRole('textbox', { name: '任务描述', exact: true })
  await expect(editor).toBeEditable()
  await expect(page.getByText('正在检查任务，第 1 段回复。', { exact: true })).toBeVisible()
  await editor.fill('你再看看后端')
  await editor.press('Enter')
  await expect(page.locator('.pending-input-text')).toHaveText('你再看看后端')
  await expect(editor).toHaveValue('')
  assert.equal(calls.length, 1, 'queued input must not interrupt the active model response')
  await page.getByRole('button', { name: '消息操作', exact: true }).click()
  await page.getByRole('menuitem', { name: '编辑消息' }).click()
  await page.getByLabel('编辑待发送消息').fill('你再检查后端的停止逻辑')
  await page
    .locator('.pending-input-editor')
    .getByRole('button', { name: '保存', exact: true })
    .click()
  await expect(page.locator('.pending-input-text')).toHaveText('你再检查后端的停止逻辑')
  await page.screenshot({ path: resolve(artifacts, 'steering-queue.png') })
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 680),
  )
  await expect(page.getByRole('button', { name: '引导', exact: true })).toBeInViewport()
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  await page.screenshot({ path: resolve(artifacts, 'steering-compact.png') })
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1440, 940),
  )
  await page.getByRole('button', { name: '引导', exact: true }).click()
  await expect.poll(() => calls.length).toBe(2)
  await expect(page.locator('.pending-input')).toHaveCount(0)
  await expect(page.locator('.message-interrupted')).toHaveCount(1)
  assert.ok(JSON.stringify(calls[1]).includes('你再检查后端的停止逻辑'))
  await editor.fill('再检查 Windows 的行为')
  await editor.press('Enter')
  await expect(page.locator('.pending-input')).toHaveCount(1)
  await page.getByRole('button', { name: '停止运行', exact: true }).click()
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toHaveCount(0)
  await expect(page.locator('.message-interrupted')).toHaveCount(2)
  await expect(page.locator('.pending-input-hint')).toContainText('已停止')
  await page.reload()
  await expect(page.locator('.pending-input-text')).toHaveText('再检查 Windows 的行为')
  await expect(page.locator('.message-interrupted')).toHaveCount(2)
  assert.equal(calls.length, 2, 'stop and reload must not automatically resume queued work')
  await page.screenshot({ path: resolve(artifacts, 'steering-stopped.png') })
  await page.getByRole('button', { name: '引导', exact: true }).click()
  await expect.poll(() => calls.length).toBe(3)
  responses[2].write(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`,
  )
  responses[2].end('data: [DONE]\n\n')
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toHaveCount(0)
  const final = await page.evaluate((id) => window.harness.getRun(id), run.id)
  assert.equal(
    final.status,
    'waiting_for_input',
    final.error ?? 'response should complete normally',
  )
  assert.equal(final.error ?? null, null)
  assert.equal(final.pending_inputs.length, 0)
  assert.equal(
    final.messages.filter((message) => message.text === '再检查 Windows 的行为').length,
    1,
  )
  assert.equal(
    final.messages.filter((message) => message.text === '你再检查后端的停止逻辑').length,
    1,
  )
  // Finishing a response must leave all unselected messages staged.
  await editor.fill('继续检查原任务')
  await editor.press('Enter')
  await expect.poll(() => calls.length).toBe(4)
  await editor.fill('还没想好，先不要发送')
  await editor.press('Enter')
  await expect(page.locator('.pending-input')).toHaveCount(1)
  await editor.fill('只发送这一条')
  await editor.press('Enter')
  await expect(page.locator('.pending-input')).toHaveCount(2)
  await editor.fill('仍在输入中的草稿')
  responses[3].write(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`,
  )
  responses[3].end('data: [DONE]\n\n')
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toHaveCount(0)
  await page.reload()
  await expect(page.locator('.pending-input')).toHaveCount(2)
  await expect(editor).toHaveValue('仍在输入中的草稿')
  const settledDrafts = await page.evaluate((id) => window.harness.getRun(id), run.id)
  assert.equal(settledDrafts.status, 'waiting_for_input', settledDrafts.error)
  assert.equal(settledDrafts.error ?? null, null)
  assert.equal(calls.length, 4, 'completion and reload must not send staged messages')
  assert.ok(!JSON.stringify(calls).includes('还没想好，先不要发送'))
  await page
    .locator('.pending-input')
    .filter({ hasText: '只发送这一条' })
    .getByRole('button', { name: '引导', exact: true })
    .click()
  await expect.poll(() => calls.length).toBe(5)
  assert.ok(JSON.stringify(calls[4]).includes('只发送这一条'))
  assert.ok(!JSON.stringify(calls[4]).includes('还没想好，先不要发送'))
  assert.ok(!JSON.stringify(calls[4]).includes('仍在输入中的草稿'))
  responses[4].write(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\n`,
  )
  responses[4].end('data: [DONE]\n\n')
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toHaveCount(0)
  await page.reload()
  await expect(page.locator('.pending-input-text')).toHaveText('还没想好，先不要发送')
  const selectedComplete = await page.evaluate((id) => window.harness.getRun(id), run.id)
  assert.equal(selectedComplete.status, 'waiting_for_input', selectedComplete.error)
  assert.equal(selectedComplete.error ?? null, null)
  assert.equal(calls.length, 5)
  await expect(editor).toHaveValue('仍在输入中的草稿')
  await page.screenshot({ path: resolve(artifacts, 'steering-staged-after-completion.png') })
  assert.deepEqual(errors, [])
  console.log(
    'PASS: explicit steering only; drafts survive completion/stop/reload; selecting one leaves others staged; 5 local model requests, no external API calls',
  )
} finally {
  await application?.close()
  for (const response of responses) response.destroy()
  await new Promise((done) => server.close(done))
}
