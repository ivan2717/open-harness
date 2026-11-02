import assert from 'node:assert/strict'
import { electronFixtureArgs } from './fixture-environment.mjs'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

// Use the production renderer and preload with recorded-shape run snapshots.
// Only this isolated Electron process has its backend IPC replaced. No user's
// data, credentials, browser, or paid model is involved.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-agent-panel-e2e-'))
const artifacts = resolve('test-results')
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
const errors = []
let application
let page
const now = Date.now() - 180_000
const timestamp = (seconds) => new Date(now + seconds * 1_000).toISOString()
const message = (id, role, text, seconds, extra = {}) => ({
  id,
  role,
  text,
  status: 'complete',
  started_at: timestamp(seconds),
  completed_at: timestamp(seconds + 1),
  ...extra,
})
const tool = (id, name, args, result, seconds) =>
  message(id, 'tool', JSON.stringify(result), seconds, {
    tool_name: name,
    tool_arguments: JSON.stringify(args),
  })
const event = (sequence, type, seconds) => ({
  sequence,
  step: sequence,
  type,
  label: type,
  detail: '',
  timestamp: timestamp(seconds),
})

try {
  await mkdir(artifacts, { recursive: true })
  application = await _electron.launch({
    executablePath: process.env.OPENHARNESS_E2E_EXECUTABLE ?? require('electron'),
    args: process.env.OPENHARNESS_E2E_EXECUTABLE ? [] : electronFixtureArgs(),
    env,
    timeout: 30_000,
  })
  page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  const seed = await page.evaluate(async () => {
    await window.harness.setPluginEnabled('builtin.llm-wiki', false)
    const created = await window.harness.createRun({
      prompt: '协作面板验收',
      model_id: 'local-demo',
    })
    await window.harness.cancelRun(created.id)
    return {
      base: await window.harness.getRun(created.id),
      config: await window.harness.config(),
      extensions: await window.harness.runExtensions(created.id),
      permissions: await window.harness.runPermissions(created.id),
      skills: await window.harness.skillCatalog(),
    }
  })
  const rootId = seed.base.id
  const firstId = 'fixture-agent-first'
  const secondId = 'fixture-agent-second'
  const nestedId = 'fixture-agent-nested'
  const makeRun = (id, title, parentId, status, messages, seconds) => ({
    ...seed.base,
    id,
    title,
    prompt: title,
    revision: 1_000_000,
    status,
    workspace_path: null,
    execution_directory: null,
    context: null,
    error: undefined,
    pending_inputs: [],
    created_at: timestamp(seconds),
    updated_at: timestamp(170),
    delegation: parentId
      ? { parent_run_id: parentId, root_run_id: rootId, task_name: title }
      : null,
    messages,
    events: [
      event(0, 'run_started', seconds),
      ...(status === 'completed' ? [event(20, 'run_completed', 169)] : []),
    ],
  })
  const rootMessages = Array.from({ length: 8 }, (_, index) => [
    message(`old-user-${index}`, 'user', `此前讨论 ${index + 1}`, index * 3),
    message(
      `old-answer-${index}`,
      'assistant',
      `保留主会话历史 ${index + 1}，查看子任务时不改变主会话。`,
      index * 3 + 1,
    ),
  ]).flat()
  const root = makeRun(
    rootId,
    '检查项目并独立复核',
    null,
    'running',
    [
      ...rootMessages,
      message('root-user', 'user', '分别核对实现与测试，主任务继续整理结果。', 100),
      message('root-intro', 'assistant', '我已把两个独立检查任务分配给子智能体。', 101),
      tool(
        'spawn-first',
        'spawn_agent',
        { task_name: '实现检查', prompt: '检查实现' },
        { agent_id: firstId, task_name: '实现检查' },
        102,
      ),
      tool(
        'spawn-second',
        'spawn_agent',
        { task_name: '测试复核', prompt: '复核测试' },
        { agent_id: secondId, task_name: '测试复核' },
        103,
      ),
      message(
        'root-progress',
        'assistant',
        Array.from(
          { length: 12 },
          (_, index) => `主任务进度 ${index + 1}：保持当前需求，等待复核结果。`,
        ).join('\n\n'),
        104,
      ),
    ],
    0,
  )
  const first = makeRun(
    firstId,
    '实现检查',
    rootId,
    'running',
    [
      message('first-user', 'user', '检查实现，保留证据，并委派边界复核。', 105),
      message(
        'first-intro',
        'assistant',
        '## 实现核对\n\n**已确认**数据与视图独立。\n\n- 核对入口\n- 核对订阅',
        106,
      ),
      tool(
        'first-read',
        'read_file',
        { path: 'src/agent.ts' },
        { content: 'CHILD_SOURCE_EVIDENCE\nexport const independent = true' },
        107,
      ),
      tool(
        'first-spawn',
        'spawn_agent',
        { task_name: '边界复核', prompt: '检查边界' },
        { agent_id: nestedId, task_name: '边界复核' },
        108,
      ),
      message('first-stream', 'assistant', '正在确认子任务更新', 109, {
        status: 'streaming',
        completed_at: undefined,
      }),
    ],
    105,
  )
  const second = makeRun(
    secondId,
    '测试复核',
    rootId,
    'completed',
    [
      message('second-user', 'user', '独立复核测试输出。', 110),
      message('second-intro', 'assistant', '先运行检查脚本，再检查完整结果。', 111),
      tool(
        'second-command',
        'run_command',
        { command: 'node verify.mjs --independent' },
        {
          status: 'completed',
          exit_code: 0,
          stdout: 'CHILD_TEST_OUTPUT\nAll checks passed',
          stderr: '',
        },
        112,
      ),
      message(
        'second-final',
        'assistant',
        '## 测试结论\n\n**全部通过**，子任务记录独立保留。\n\n```text\n' +
          'long-evidence-'.repeat(45) +
          '\n```',
        168,
      ),
    ],
    110,
  )
  const nested = makeRun(
    nestedId,
    '边界复核',
    firstId,
    'completed',
    [
      message('nested-user', 'user', '检查切换和关闭面板边界。', 120),
      message('nested-final', 'assistant', '## 嵌套检查\n\n关闭视图不停止任务。NESTED_RESULT', 168),
    ],
    120,
  )

  await application.evaluate(
    ({ ipcMain }, fixture) => {
      globalThis.agentPanelFixture = {
        ...fixture,
        runs: new Map(fixture.runs.map((run) => [run.id, run])),
        subscriptions: new Map(),
        calls: [],
        lifecycle: [],
      }
      ipcMain.removeHandler('openharness:invoke')
      ipcMain.handle('openharness:invoke', (_event, request) => {
        const state = globalThis.agentPanelFixture
        state.calls.push(request)
        if (request.operation === 'config') return state.config
        if (request.operation === 'listRuns') return [...state.runs.values()]
        if (request.operation === 'getRun') {
          const run = state.runs.get(request.id)
          if (!run) throw new Error('Fixture run not found')
          return run
        }
        if (request.operation === 'runExtensions') return state.extensions
        if (request.operation === 'runPermissions') return state.permissions
        if (request.operation === 'skillCatalog') return state.skills
        if (request.operation === 'cancelRun')
          throw new Error('Unexpected cancellation during inspection')
        throw new Error(`Unexpected fixture API: ${request.operation}`)
      })
      ipcMain.removeAllListeners('openharness:watch:start')
      ipcMain.removeAllListeners('openharness:watch:stop')
      ipcMain.on('openharness:watch:start', (event, request) => {
        const state = globalThis.agentPanelFixture
        state.lifecycle.push({ action: 'start', id: request.id, token: request.token })
        state.subscriptions.set(request.token, { ...request, sender: event.sender })
        event.sender.send('openharness:watch:event', {
          token: request.token,
          snapshot: state.runs.get(request.id),
        })
      })
      ipcMain.on('openharness:watch:stop', (_event, token) => {
        const state = globalThis.agentPanelFixture
        const entry = state.subscriptions.get(token)
        state.lifecycle.push({ action: 'stop', id: entry?.id, token })
        state.subscriptions.delete(token)
      })
    },
    { ...seed, runs: [root, first, second, nested] },
  )
  const subscribed = () =>
    application.evaluate(() =>
      [...globalThis.agentPanelFixture.subscriptions.values()].map((entry) => entry.id).sort(),
    )
  const emit = (run) =>
    application.evaluate((_electron, run) => {
      const state = globalThis.agentPanelFixture
      state.runs.set(run.id, run)
      for (const entry of state.subscriptions.values())
        if (entry.id === run.id)
          entry.sender.send('openharness:watch:event', {
            token: entry.token,
            snapshot: run,
          })
    }, run)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, rootId)
  const main = page.locator('.main-pane')
  const panel = page.getByRole('complementary', { name: '子智能体', exact: true })
  await expect(main.locator('.agent-activity-link')).toHaveCount(2)
  await expect.poll(subscribed).toEqual([rootId])
  const draft = main.locator('textarea')
  await draft.fill('这是一条尚未发送的主会话草稿')
  const rootUrl = page.url()
  const openFirst = main.getByRole('button', { name: '查看协作任务：实现检查', exact: true })
  await openFirst.scrollIntoViewIfNeeded()
  const scroll = main.locator('.conversation-scroll')
  const savedScroll = await scroll.evaluate((element) => element.scrollTop)
  assert.ok(savedScroll > 0, 'Fixture has a scrollable main conversation')
  await openFirst.click()
  await expect(panel).toBeVisible()
  await expect(panel.getByRole('heading', { name: '实现检查', exact: true })).toBeVisible()
  await expect.poll(subscribed).toEqual([firstId, rootId].sort())
  assert.equal(page.url(), rootUrl)
  await expect(draft).toHaveValue('这是一条尚未发送的主会话草稿')
  await expect
    .poll(() => scroll.evaluate((element) => element.scrollTop))
    .toBeCloseTo(savedScroll, 0)
  await expect(panel.getByRole('heading', { name: '实现核对' })).toBeVisible()
  await expect(panel.locator('strong').filter({ hasText: '已确认' })).toBeVisible()
  await expect(panel.locator('.message-user')).toContainText('委派边界复核')
  const read = panel.locator('.tool-activity').filter({ hasText: 'src/agent.ts' })
  await read.locator(':scope > summary').click()
  await expect(read).toContainText('CHILD_SOURCE_EVIDENCE')
  await expect(panel.getByRole('button', { name: '停止子任务', exact: true })).toBeVisible()
  await panel.locator('.conversation-scroll').evaluate((element) => {
    element.scrollTop = 0
  })
  await page.screenshot({ path: resolve(artifacts, 'agent-panel-running.png') })

  const firstUpdated = {
    ...first,
    revision: first.revision + 1,
    messages: first.messages.map((entry) =>
      entry.id === 'first-stream'
        ? { ...entry, text: 'CHILD_STREAM_ONLY：更新只出现在子智能体面板。' }
        : entry,
    ),
  }
  await emit(firstUpdated)
  await expect(panel).toContainText('CHILD_STREAM_ONLY')
  await expect(main).not.toContainText('CHILD_STREAM_ONLY')
  await expect(draft).toHaveValue('这是一条尚未发送的主会话草稿')

  // Nested activities open within the same inspector, never navigate the main run.
  await panel.getByRole('button', { name: '查看协作任务：边界复核', exact: true }).click()
  await expect(panel.getByRole('heading', { name: '边界复核', exact: true })).toBeVisible()
  await expect(panel).toContainText('NESTED_RESULT')
  await expect.poll(subscribed).toEqual([nestedId, rootId].sort())
  assert.equal(page.url(), rootUrl)
  await panel.getByRole('button', { name: '返回子智能体列表' }).click()
  await expect(panel.locator('.agent-panel-list > button')).toHaveCount(3)
  await expect.poll(subscribed).toEqual([rootId])
  await panel
    .locator('.agent-panel-list')
    .getByRole('button', { name: /测试复核/ })
    .click()
  await expect(panel.getByRole('heading', { name: '测试复核', exact: true })).toBeVisible()
  await expect.poll(subscribed).toEqual([secondId, rootId].sort())
  await expect(panel.getByRole('heading', { name: '测试结论' })).toBeVisible()
  await expect(panel.locator('.turn-work')).toHaveJSProperty('open', false)
  await panel.locator('.turn-work > summary').click()
  const command = panel.locator('.tool-activity')
  await command.locator(':scope > summary').click()
  await expect(command).toContainText('CHILD_TEST_OUTPUT')
  await expect(command).toContainText('node verify.mjs --independent')
  await expect(panel.getByRole('button', { name: '停止子任务' })).toHaveCount(0)
  await page.screenshot({ path: resolve(artifacts, 'agent-panel-completed.png') })
  await panel.getByRole('button', { name: '关闭子智能体面板' }).click()
  await expect(panel).toHaveCount(0)
  await expect.poll(subscribed).toEqual([rootId])
  assert.equal(page.url(), rootUrl)
  await expect(draft).toHaveValue('这是一条尚未发送的主会话草稿')

  // The independent task list also opens the panel, and Escape only closes it.
  await main.locator('.collaboration-tasks > summary').click()
  await main.locator('.collaboration-task-link').filter({ hasText: '实现检查' }).click()
  await expect(panel).toContainText('CHILD_STREAM_ONLY')
  await expect.poll(subscribed).toEqual([firstId, rootId].sort())
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await expect.poll(subscribed).toEqual([rootId])
  await main.locator('.collaboration-task-link').filter({ hasText: '测试复核' }).click()
  await application.evaluate(({ BrowserWindow }) => {
    BrowserWindow.getAllWindows()[0].setSize(1000, 700)
  })
  await expect(panel).toBeVisible()
  await expect.poll(() => page.evaluate(() => innerWidth)).toBe(1000)
  const bounds = await panel.boundingBox()
  assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 1001)
  assert.ok(bounds.y >= 0 && bounds.y + bounds.height <= 701)
  const overflow = await page.evaluate(() => ({
    document: document.documentElement.scrollWidth - innerWidth,
    panel:
      document.querySelector('.agent-panel').scrollWidth -
      document.querySelector('.agent-panel').clientWidth,
  }))
  assert.ok(overflow.document <= 1 && overflow.panel <= 1, 'Narrow panel stays within the window')
  await page.screenshot({ path: resolve(artifacts, 'agent-panel-narrow.png') })
  await panel.getByRole('button', { name: '关闭子智能体面板' }).click()
  await expect.poll(subscribed).toEqual([rootId])
  await expect(draft).toHaveValue('这是一条尚未发送的主会话草稿')
  assert.equal(page.url(), rootUrl)

  const audit = await application.evaluate(() => ({
    calls: globalThis.agentPanelFixture.calls,
    lifecycle: globalThis.agentPanelFixture.lifecycle,
  }))
  assert.ok(
    !audit.calls.some((call) =>
      ['cancelRun', 'resumeRun', 'submitInput', 'updatePendingInput'].includes(call.operation),
    ),
    'Inspecting, switching and closing never stop or send work',
  )
  const childSubscriptions = audit.lifecycle.filter(
    (entry) => entry.action === 'start' && entry.id !== rootId,
  )
  const stoppedTokens = new Set(
    audit.lifecycle.filter((entry) => entry.action === 'stop').map((entry) => entry.token),
  )
  assert.equal(childSubscriptions.length, 5)
  assert.ok(
    childSubscriptions.every((entry) => stoppedTokens.has(entry.token)),
    'Each child inspection releases its own watch subscription',
  )
  assert.deepEqual(errors, [])
  console.log(
    'PASS: child activity/list navigation, full Markdown and tools, isolated streaming, nested agents, draft/URL/scroll retention, watch cleanup, close-without-cancel and narrow layout',
  )
} catch (error) {
  await page?.screenshot({ path: resolve(artifacts, 'agent-panel-failure.png') }).catch(() => {})
  throw error
} finally {
  await application?.close()
  await rm(directory, { recursive: true, force: true })
}
