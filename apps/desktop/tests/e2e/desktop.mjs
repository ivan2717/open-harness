import assert from 'node:assert/strict'
import { mkdtemp, mkdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const dataDirectory = await mkdtemp(resolve(tmpdir(), 'openharness-desktop-e2e-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const errors = []
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: dataDirectory }
delete env.ELECTRON_RUN_AS_NODE
let application

async function launch() {
  application = await _electron.launch({
    executablePath: process.env.OPENHARNESS_E2E_EXECUTABLE ?? require('electron'),
    args: process.env.OPENHARNESS_E2E_EXECUTABLE ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeVisible()
  await page.evaluate(() => window.harness.config())
  assert.deepEqual(
    await page.evaluate(() => ({
      node: typeof globalThis.process,
      require: typeof globalThis.require,
    })),
    { node: 'undefined', require: 'undefined' },
  )
  return page
}

try {
  let page = await launch()
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await expect(page.locator('.inspector')).toHaveCount(0)
  await expect(page.getByRole('link', { name: '添加模型', exact: true })).toBeVisible()
  await expect(page.getByLabel('选择模型')).toHaveCount(0)
  await expect(page.getByText(/Local demo/)).toHaveCount(0)
  await expect(page.getByRole('button', { name: '试试上下文演示' })).toHaveCount(0)
  await expect(page.getByText('仅对话', { exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '更多操作', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toHaveCount(0)
  await expect(page.getByRole('link', { name: '设置', exact: true })).toHaveCount(1)
  await expect(
    page.locator('.composer-stack').getByRole('link', { name: '设置', exact: true }),
  ).toHaveCount(0)
  await page.screenshot({ path: resolve(artifacts, 'desktop-start.png') })
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 680),
  )
  await expect.poll(() => page.evaluate(() => window.innerWidth)).toBe(1000)
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeInViewport()
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth),
    false,
    'the minimum desktop window must not overflow horizontally',
  )
  await page.screenshot({ path: resolve(artifacts, 'desktop-start-compact.png') })
  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1440, 940),
  )
  await expect.poll(() => page.evaluate(() => window.innerWidth)).toBe(1440)
  const draft = 'Keep this real task draft while checking the developer demo.'
  const description = page.getByRole('textbox', { name: '任务描述', exact: true })
  const shortHeight = await description.evaluate((element) => element.clientHeight)
  await description.fill(Array.from({ length: 20 }, (_, index) => `Task line ${index}`).join('\n'))
  await expect
    .poll(() => description.evaluate((element) => element.clientHeight))
    .toBeGreaterThan(shortHeight)
  assert.ok((await description.evaluate((element) => element.clientHeight)) <= 220)
  assert.equal(await description.evaluate((element) => getComputedStyle(element).resize), 'none')
  await description.fill(draft)
  await expect.poll(() => description.evaluate((element) => element.clientHeight)).toBe(shortHeight)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeEditable()
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()
  await page.getByRole('textbox', { name: '任务描述', exact: true }).press('Enter')
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await page.getByRole('link', { name: '添加模型', exact: true }).click()
  await expect(page.getByRole('heading', { name: '默认模型', exact: true })).toHaveCount(0)
  await expect(page.locator('.model-advanced > summary')).toBeVisible()
  await expect(page.getByLabel('上下文窗口（tokens）')).toBeHidden()
  await expect(page.getByLabel('单次输出上限（tokens）')).toBeHidden()
  await expect(page.getByRole('button', { name: '运行上下文演示', exact: true })).toBeHidden()
  await page.getByRole('link', { name: '设置', exact: true }).click()
  await page.getByText('开发调试', { exact: true }).click()
  await page.getByRole('button', { name: '运行上下文演示', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const runId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toBeVisible()
  await expect(
    page.locator('.workspace-heading').getByRole('img', { name: '运行中' }),
  ).toBeVisible()
  await expect(
    page.locator('.run-link.is-active').getByRole('img', { name: '运行中' }),
  ).toBeVisible()
  await expect(page.locator('.run-link.is-active .animate-spin')).toHaveCSS(
    'animation-name',
    'spin',
  )
  await page.screenshot({ path: resolve(artifacts, 'desktop-activity-running.png') })
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, runId), {
      timeout: 45_000,
    })
    .toBe('completed')
  const complete = await page.evaluate((id) => window.harness.getRun(id), runId)
  await expect(page.locator('.workspace-heading').getByRole('img', { name: '空闲' })).toBeVisible()
  await expect(page.locator('.run-link.is-active').getByRole('img', { name: '空闲' })).toBeVisible()
  await expect(page.locator('.run-link-meta').filter({ hasText: /已完成|等待回复/ })).toHaveCount(0)
  await expect(page.locator('.run-status-label, .run-result, .finished-note')).toHaveCount(0)
  assert.equal(complete.model_id, 'local-demo')
  const prompt = complete.prompt
  assert.ok(
    complete.context.compaction_count > 0,
    'the real context manager must create a checkpoint',
  )
  assert.ok(
    complete.context.checkpoints.every((item) => item.before_tokens > item.after_tokens),
    'checkpoints must report actual token reduction',
  )
  assert.ok(
    complete.events.some((event) => event.type === 'tool_call_succeeded'),
    'the runtime must execute tools',
  )
  assert.ok(
    complete.messages.some(
      (message) => message.role === 'assistant' && message.status === 'complete',
    ),
  )
  await page.getByRole('button', { name: '打开运行详情', exact: true }).click()
  await expect(page.getByText('上下文压缩 1', { exact: true })).toBeVisible()
  await page.getByText('上下文压缩 1', { exact: true }).click()
  await page.screenshot({ path: resolve(artifacts, 'desktop-context.png') })
  await page.getByRole('tab', { name: /时间线/ }).click()
  await page.screenshot({ path: resolve(artifacts, 'desktop-timeline.png') })

  await page.getByRole('link', { name: /新建会话/ }).click()
  await expect(page.locator('.inspector')).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(draft)
  await expect(page.getByRole('link', { name: '添加模型', exact: true })).toBeVisible()
  const cancelled = await page.evaluate(() =>
    window.harness.createRun({ prompt: '验证用户取消能停止执行。', model_id: 'local-demo' }),
  )
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, cancelled.id)
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toBeVisible()
  const cancelledId = cancelled.id
  await page.getByRole('button', { name: '停止运行', exact: true }).click()
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, cancelledId))
    .toBe('cancelled')
  await expect(page.locator('.run-link.is-active').getByRole('img', { name: '空闲' })).toBeVisible()
  await expect(page.getByRole('button', { name: '继续任务', exact: true })).toBeVisible()

  await page.getByRole('link', { name: /新建会话/ }).click()
  const resumed = await page.evaluate(() =>
    window.harness.createRun({ prompt: '/ask 验证补充输入后继续运行。', model_id: 'local-demo' }),
  )
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, resumed.id)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeEditable()
  await expect(page.locator('.run-link.is-active').getByRole('img', { name: '空闲' })).toBeVisible()
  await expect(page.locator('.composer-hint, .composer-waiting')).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveAttribute(
    'placeholder',
    '描述任务，或直接提问…',
  )
  await page.screenshot({ path: resolve(artifacts, 'desktop-activity-idle.png') })
  const resumedId = resumed.id
  await page.getByRole('textbox', { name: '任务描述', exact: true }).fill('继续执行本地诊断。')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, resumedId), {
      timeout: 45_000,
    })
    .toBe('completed')

  await application.close()
  application = undefined
  page = await launch()
  const restored = await page.evaluate((id) => window.harness.getRun(id), runId)
  assert.equal(restored.status, 'completed')
  assert.equal(restored.events.length, complete.events.length)
  assert.equal(restored.context.compaction_count, complete.context.compaction_count)
  assert.equal(
    (await page.evaluate((id) => window.harness.getRun(id), cancelledId)).status,
    'cancelled',
  )
  await page.getByRole('link', { name: new RegExp(prompt.slice(0, 8)) }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain(runId)
  await page.getByRole('link', { name: '设置', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toContain('/settings')
  await page.screenshot({ path: resolve(artifacts, 'desktop-settings.png') })
  assert.deepEqual(errors, [], 'renderer must not report uncaught exceptions')
  console.log(
    JSON.stringify(
      {
        passed: true,
        runId,
        compactions: complete.context.compaction_count,
        events: complete.events.length,
        dataDirectory,
        artifacts,
      },
      null,
      2,
    ),
  )
} finally {
  await application?.close()
}
