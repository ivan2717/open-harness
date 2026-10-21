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
  await expect
    .poll(() => page.evaluate(async () => (await window.harness.config()).default_model))
    .toBe('local-demo')
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
  await page.getByRole('button', { name: '试试上下文演示' }).click()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeFocused()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    /上下文诊断演示/,
  )
  const prompt = '观察一次完整的上下文压缩，保留原始工具事件，并展示压缩前后的预算。'
  await page.getByRole('textbox', { name: '任务描述', exact: true }).fill(prompt)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const runId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toBeVisible()
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, runId), {
      timeout: 45_000,
    })
    .toBe('completed')
  const complete = await page.evaluate((id) => window.harness.getRun(id), runId)
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
  await page.getByRole('button', { name: '打开运行详情' }).click()
  await expect(page.getByText('上下文压缩 1', { exact: true })).toBeVisible()
  await page.getByText('上下文压缩 1', { exact: true }).click()
  await page.screenshot({ path: resolve(artifacts, 'desktop-context.png') })
  await page.getByRole('tab', { name: /时间线/ }).click()
  await page.screenshot({ path: resolve(artifacts, 'desktop-timeline.png') })

  await page.getByRole('link', { name: /新建会话/ }).click()
  await expect(page.locator('.inspector')).toHaveCount(0)
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('验证用户取消能停止执行。')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toBeVisible()
  const cancelledId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await page.getByRole('button', { name: '停止运行', exact: true }).click()
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, cancelledId))
    .toBe('cancelled')

  await page.getByRole('link', { name: /新建会话/ }).click()
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('/ask 验证补充输入后继续运行。')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect(page.getByRole('textbox', { name: '补充信息', exact: true })).toBeVisible()
  const resumedId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await page.getByRole('textbox', { name: '补充信息', exact: true }).fill('继续执行本地诊断。')
  await page.getByRole('button', { name: '提交补充信息', exact: true }).click()
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
  await page.getByRole('link', { name: /工作区设置/ }).click()
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
