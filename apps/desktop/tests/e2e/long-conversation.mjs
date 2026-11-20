import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'
import { electronFixtureArgs } from './fixture-environment.mjs'

// A tool-only model response contributes an invisible assistant row as well as
// its tool row. Exercise that production shape across the bounded live window.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-long-conversation-e2e-'))
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
    const run = await window.harness.createRun({
      prompt: '连续检查大量操作，保留问题和阅读位置',
      model_id: 'local-demo',
    })
    await window.harness.cancelRun(run.id)
    return window.harness.getRun(run.id)
  })
  await application.evaluate(({ ipcMain }, base) => {
    const time = (index) => new Date(Date.UTC(2026, 10, 1, 0, 0, index)).toISOString()
    const message = (id, role, text, index, extra = {}) => ({
      id,
      role,
      text,
      status: 'complete',
      started_at: time(index),
      completed_at: time(index + 1),
      ...extra,
    })
    const state = (globalThis.longConversationFixture = {
      run: {
        ...base,
        revision: 1_000_000,
        status: 'running',
        context: null,
        error: undefined,
        pending_inputs: [],
        messages: [
          message('event-0', 'user', '连续检查大量操作，保留问题和阅读位置', 0),
          message('event-1', 'assistant', '我会逐项检查操作结果，并保留最初的说明。', 1),
        ],
        events: [],
      },
      subscriptions: new Map(),
      operations: 0,
    })
    state.append = () => {
      const index = ++state.operations
      state.run.messages.push(message(`event-${index * 4}`, 'assistant', '', index * 5))
      state.run.messages.push(
        message(
          `tool-${index}`,
          'tool',
          JSON.stringify({
            status: 'completed',
            exit_code: 0,
            stdout: `检查结果 ${index}`,
          }),
          index * 5 + 1,
          {
            tool_name: 'run_command',
            tool_arguments: JSON.stringify({ command: `echo check-${index}` }),
          },
        ),
      )
    }
    for (let index = 0; index < 49; index++) state.append()
    state.view = (before) => {
      const end =
        before === undefined
          ? state.run.messages.length
          : state.run.messages.findIndex((item) => item.id === before)
      if (end < 0) throw new Error('Invalid fixture cursor')
      const start = Math.max(0, end - 100)
      return {
        ...state.run,
        messages: state.run.messages.slice(start, end),
        history: {
          before: start ? state.run.messages[start].id : null,
          total_messages: state.run.messages.length,
          total_events: 0,
          events_before: null,
          ...(start > 0 && state.run.messages[start]?.role !== 'user'
            ? {
                leading_turn: {
                  id: 'event-0',
                  user: state.run.messages[0],
                  preview: state.run.messages
                    .slice(0, start)
                    .findLast((item) => item.role === 'assistant' && item.text),
                  started_at: state.run.messages[0].started_at,
                  completed_at:
                    state.run.status === 'completed'
                      ? state.run.messages.at(-1).completed_at
                      : undefined,
                },
              }
            : {}),
        },
      }
    }
    state.emit = () => {
      state.run.revision++
      for (const [token, sender] of state.subscriptions)
        sender.send('openharness:watch:event', { token, snapshot: state.view() })
    }
    const original = ipcMain._invokeHandlers.get('openharness:invoke')
    ipcMain.removeHandler('openharness:invoke')
    ipcMain.handle('openharness:invoke', (event, request) => {
      if (request.id === state.run.id && request.operation === 'getRun') return state.view()
      if (request.id === state.run.id && request.operation === 'getRunPage')
        return state.view(request.before)
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
  const openRun = () =>
    page.evaluate((id) => {
      window.location.hash = `/runs/${id}`
    }, base.id)
  const group = page.locator('.tool-activity-group')
  const scroll = page.locator('.conversation-scroll')
  const historyButton = page.getByRole('button', { name: '查看更早的消息', exact: true })
  const tool = (index) =>
    page.locator('.tool-activity').filter({
      has: page.locator('.activity-target', { hasText: new RegExp(`^echo check-${index}$`) }),
    })
  async function checkContext() {
    await expect(page.locator('.message-user')).toHaveCount(1)
    await expect(page.locator('.message-user')).toContainText('连续检查大量操作')
    await expect(page.locator('.message-assistant')).toHaveCount(1)
    await expect(page.locator('.message-assistant')).toContainText('保留最初的说明')
  }
  await openRun()
  await checkContext()
  await expect(group.locator('.activity-count')).toHaveText('49 次操作')
  await group.locator(':scope > summary').click()
  await expect(group).toHaveJSProperty('open', true)
  await tool(20).locator(':scope > summary').click()
  await expect(tool(20)).toHaveJSProperty('open', true)
  await scroll.evaluate(async (element) => {
    element.scrollTop = 300
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))
  })
  const readingTop = (await tool(20).boundingBox()).y
  await page.evaluate(() => {
    globalThis.previousConversationTurn = document.querySelector('.conversation-turn')
  })
  for (let count = 50; count <= 52; count++) {
    await application.evaluate(() => {
      globalThis.longConversationFixture.append()
      globalThis.longConversationFixture.emit()
    })
    await checkContext()
    await expect(group.locator('.activity-count')).toHaveText(`${count} 次操作`)
    await expect(group).toHaveJSProperty('open', true)
    await expect(tool(20)).toHaveJSProperty('open', true)
    assert.ok(
      Math.abs((await tool(20).boundingBox()).y - readingTop) < 8,
      'New operations preserve the position being read',
    )
    assert.equal(
      await page.evaluate(() => globalThis.previousConversationTurn.isConnected),
      true,
      'Sliding the transport window does not recreate the current turn',
    )
  }
  await expect(historyButton).toHaveCount(0)
  await page.screenshot({ path: resolve('test-results/long-conversation-live.png') })

  // Reopening starts with only the latest page; explicit metadata provides the
  // original question and prose without fetching the entire single-turn log.
  await page.evaluate(() => {
    window.location.hash = '/'
  })
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await application.evaluate(() => {
    const state = globalThis.longConversationFixture
    while (state.operations < 150) state.append()
    state.run.revision++
  })
  await openRun()
  await expect(group.locator('.activity-count')).toHaveText('50 次操作')
  await checkContext()
  await expect(historyButton).toBeVisible()
  await group.locator(':scope > summary').click()
  await tool(105).locator(':scope > summary').click()
  await expect(tool(105)).toHaveJSProperty('open', true)
  await tool(105).scrollIntoViewIfNeeded()
  const oldTop = (await tool(105).boundingBox()).y
  await page.evaluate(() => {
    globalThis.previousConversationTurn = document.querySelector('.conversation-turn')
  })
  // Invoke loading without Playwright first scrolling to the top button: the
  // assertion measures the application's own history anchoring behavior.
  await historyButton.evaluate((element) => element.click())
  await expect(group.locator('.activity-count')).toHaveText('100 次操作')
  await expect(group).toHaveJSProperty('open', true)
  await expect(tool(105)).toHaveJSProperty('open', true)
  const newTop = (await tool(105).boundingBox()).y
  assert.ok(
    Math.abs(newTop - oldTop) < 8,
    `Prepending within a turn preserves the visible tool message (before=${oldTop}, after=${newTop})`,
  )
  assert.equal(await page.evaluate(() => globalThis.previousConversationTurn.isConnected), true)
  await checkContext()
  await historyButton.evaluate((element) => element.click())
  await expect(group.locator('.activity-count')).toHaveText('150 次操作')
  await historyButton.evaluate((element) => element.click())
  await expect(historyButton).toHaveCount(0)
  await expect(page.locator('.tool-activity')).toHaveCount(150)
  await expect(tool(105)).toHaveJSProperty('open', true)
  await checkContext()
  const targets = await page.locator('.tool-activity > summary .activity-target').allTextContents()
  assert.equal(new Set(targets).size, 150, 'Earlier pages do not duplicate tool operations')
  await page.screenshot({ path: resolve('test-results/long-conversation-history.png') })

  await application.evaluate(() => {
    const state = globalThis.longConversationFixture
    state.run.status = 'completed'
    state.run.messages.push({
      id: 'event-1000',
      role: 'assistant',
      text: '150 次操作检查完成，历史与上下文都已保留。',
      status: 'complete',
      started_at: '2026-11-01T00:15:00Z',
      completed_at: '2026-11-01T00:15:01Z',
    })
    state.emit()
  })
  await expect(page.locator('.turn-answer')).toContainText('150 次操作检查完成')
  await expect(page.locator('.turn-work')).toHaveJSProperty('open', false)
  await expect(page.locator('.run-progress')).toHaveCount(0)
  await page.locator('.turn-work > summary').click()
  await expect(group.locator('.activity-count')).toHaveText('150 次操作')
  await expect(page.locator('.message-user')).toHaveCount(1)

  // A page can expose earlier prose and another group inside the same turn.
  // The already visible group must keep its identity when its preview becomes
  // an ordinary loaded message and earlier groups are inserted before it.
  await page.evaluate(() => {
    window.location.hash = '/'
  })
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  const mixedRunId = await page.evaluate(async () => {
    const run = await window.harness.createRun({
      prompt: '包含阶段说明的长历史',
      model_id: 'local-demo',
    })
    await window.harness.cancelRun(run.id)
    return run.id
  })
  await application.evaluate((_electron, id) => {
    const state = globalThis.longConversationFixture
    state.run.id = id
    state.run.messages.pop()
    const index = state.run.messages.findIndex((item) => item.id === 'event-300')
    state.run.messages.splice(index, 0, {
      id: 'event-299',
      role: 'assistant',
      text: '阶段说明：继续检查剩余操作。',
      status: 'complete',
      started_at: '2026-11-01T00:06:14Z',
      completed_at: '2026-11-01T00:06:15Z',
    })
    state.run.status = 'running'
    state.run.revision++
  }, mixedRunId)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, mixedRunId)
  await expect(group).toHaveCount(1)
  await expect(group.locator('.activity-count')).toHaveText('50 次操作')
  await expect(page.locator('.message-assistant')).toHaveCount(1)
  await expect(page.locator('.message-assistant')).toContainText('阶段说明')
  await group.locator(':scope > summary').click()
  await tool(105).locator(':scope > summary').click()
  await expect(tool(105)).toHaveJSProperty('open', true)
  const mixedTop = (await tool(105).boundingBox()).y
  await historyButton.evaluate((element) => element.click())
  await expect(group).toHaveCount(2)
  await expect(group.last().locator('.activity-count')).toHaveText('76 次操作')
  await expect(group.last()).toHaveJSProperty('open', true)
  await expect(tool(105)).toHaveJSProperty('open', true)
  await expect(page.locator('.message-assistant')).toHaveCount(2)
  await expect(page.locator('.message-user')).toHaveCount(1)
  assert.ok(
    Math.abs((await tool(105).boundingBox()).y - mixedTop) < 8,
    'Earlier prose and groups preserve the tool message being read',
  )
  await page.screenshot({ path: resolve('test-results/long-conversation-mixed-history.png') })
  assert.deepEqual(errors, [])
  console.log(
    'PASS: long single-turn live history, persistent expansion and scroll, bounded reopening context, within-turn paging and final answer',
  )
} catch (error) {
  await page
    ?.screenshot({ path: resolve('test-results/long-conversation-failure.png') })
    .catch(() => {})
  throw error
} finally {
  await application?.close()
  await rm(directory, { recursive: true, force: true })
}
