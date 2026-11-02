import assert from 'node:assert/strict'
import { electronFixtureArgs } from './fixture-environment.mjs'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

// Recorded-shape pages exercise the real Electron preload and renderer. Backend
// pagination/serialization is separately covered by the API's RunView tests.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-history-e2e-'))
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application
let page
const errors = []
try {
  await mkdir(resolve('test-results'), { recursive: true })
  application = await _electron.launch({
    executablePath: require('electron'),
    args: electronFixtureArgs(),
    env,
  })
  page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  const base = await page.evaluate(async () => {
    await window.harness.setPluginEnabled('builtin.llm-wiki', false)
    const run = await window.harness.createRun({ prompt: '历史分页验收', model_id: 'local-demo' })
    await window.harness.cancelRun(run.id)
    return window.harness.getRun(run.id)
  })
  await application.evaluate(({ ipcMain }, base) => {
    const time = (index) => new Date(Date.UTC(2026, 10, 1, 0, 0, index)).toISOString()
    const messages = Array.from({ length: 600 }, (_, i) => ({
      id: `event-${i}`,
      role: i % 2 ? 'assistant' : 'user',
      text: `历史记录 ${i}`,
      status: 'complete',
      started_at: time(i),
      completed_at: time(i + 1),
    }))
    const events = Array.from({ length: 900 }, (_, i) => ({
      sequence: i,
      step: i,
      type: 'user_input',
      label: `历史事件 ${i}`,
      detail: '',
      timestamp: time(i),
    }))
    const state = (globalThis.historyFixture = {
      run: {
        ...base,
        revision: 1_000_000,
        status: 'completed',
        messages,
        events,
        context: null,
        error: undefined,
        pending_inputs: [],
      },
      subscriptions: new Map(),
      calls: [],
    })
    state.view = (before) => {
      const run = state.run
      const end =
        before === undefined ? run.messages.length : run.messages.findIndex((m) => m.id === before)
      if (end < 0) throw new Error('Invalid fixture cursor')
      const start = Math.max(0, end - 100)
      return {
        ...run,
        messages: run.messages.slice(start, end),
        events: run.events.slice(-300),
        history: {
          before: start > 0 ? run.messages[start].id : null,
          total_messages: run.messages.length,
          total_events: run.events.length,
          events_before: run.events.length > 300 ? run.events.at(-300).sequence : null,
        },
      }
    }
    const original = ipcMain._invokeHandlers.get('openharness:invoke')
    ipcMain.removeHandler('openharness:invoke')
    ipcMain.handle('openharness:invoke', (event, request) => {
      state.calls.push(request)
      if (request.id !== state.run.id) return original(event, request)
      if (request.operation === 'getRun') return state.view()
      if (request.operation === 'getRunPage') return state.view(request.before)
      if (request.operation === 'getRunEventsPage') {
        const events = state.run.events.filter((e) => e.sequence < request.before)
        return {
          events: events.slice(-300),
          before: events.length > 300 ? events.at(-300).sequence : null,
        }
      }
      return original(event, request)
    })
    ipcMain.removeAllListeners('openharness:watch:start')
    ipcMain.removeAllListeners('openharness:watch:stop')
    ipcMain.on('openharness:watch:start', (event, request) => {
      if (request.id !== state.run.id) return
      state.subscriptions.set(request.token, event.sender)
      event.sender.send('openharness:watch:event', { token: request.token, snapshot: state.view() })
    })
    ipcMain.on('openharness:watch:stop', (_event, token) => state.subscriptions.delete(token))
  }, base)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, base.id)
  await expect(page.locator('.message-user')).toHaveCount(50)
  await expect(page.locator('.conversation-scroll')).not.toContainText('历史记录 0')
  await expect(page.locator('.conversation-scroll')).toContainText('历史记录 599')
  const historyButton = page.getByRole('button', { name: '查看更早的消息', exact: true })
  await historyButton.scrollIntoViewIfNeeded()
  const anchor = page.locator('.message-user').first()
  const oldTop = (await anchor.boundingBox()).y
  await historyButton.click()
  await expect(page.locator('.message-user')).toHaveCount(100)
  const newTop = (
    await page.locator('.message-user').filter({ hasText: '历史记录 500' }).boundingBox()
  ).y
  assert.ok(Math.abs(newTop - oldTop) < 8, 'Prepending history preserves the visible anchor')
  for (let count = 150; count <= 300; count += 50) {
    await historyButton.click()
    await expect(page.locator('.message-user')).toHaveCount(count)
  }
  await expect(historyButton).toHaveCount(0)
  await expect(page.locator('.message-user').first()).toContainText('历史记录 0')
  const ids = await page.locator('.message-user').allTextContents()
  assert.equal(new Set(ids).size, 300, 'Pages do not duplicate messages')

  await application.evaluate(() => {
    const state = globalThis.historyFixture
    state.run.revision++
    state.run.status = 'running'
    state.run.messages.push({
      id: 'live-stream',
      role: 'assistant',
      text: '临时流式片段',
      status: 'streaming',
    })
    for (const [token, sender] of state.subscriptions)
      sender.send('openharness:watch:event', { token, snapshot: state.view() })
  })
  await expect(page.locator('.conversation-scroll')).toContainText('临时流式片段')
  await application.evaluate(() => {
    const state = globalThis.historyFixture
    state.run.revision++
    state.run.status = 'completed'
    state.run.messages.splice(-1, 1, {
      id: 'event-900',
      role: 'assistant',
      text: '最终流式回答',
      status: 'complete',
    })
    for (const [token, sender] of state.subscriptions)
      sender.send('openharness:watch:event', { token, snapshot: state.view() })
  })
  await expect(page.locator('.conversation-scroll')).not.toContainText('临时流式片段')
  await expect(page.locator('.conversation-scroll')).toContainText('最终流式回答')
  await expect(page.locator('.message-user')).toHaveCount(300)
  if (!(await page.getByRole('tab', { name: /时间线/ }).isVisible())) {
    await page.getByRole('button', { name: '打开运行详情' }).click()
  }
  await page.getByRole('tab', { name: /时间线/ }).click()
  await expect(page.locator('.timeline > li')).toHaveCount(300)
  await page.getByRole('button', { name: '查看更早的事件' }).click()
  await expect(page.locator('.timeline > li')).toHaveCount(600)
  await page.getByRole('button', { name: '查看更早的事件' }).click()
  await expect(page.locator('.timeline > li')).toHaveCount(900)
  await expect(page.getByRole('button', { name: '查看更早的事件' })).toHaveCount(0)
  await page.screenshot({ path: resolve('test-results/history-pagination.png') })
  assert.deepEqual(errors, [])
  console.log(
    'PASS: bounded initial history, complete paging, stable scroll anchor, live/final reconciliation and independently paged events',
  )
} catch (error) {
  await page?.screenshot({ path: resolve('test-results/history-failure.png') }).catch(() => {})
  throw error
} finally {
  await application?.close()
  await rm(directory, { recursive: true, force: true })
}
