import assert from 'node:assert/strict'
import { electronFixtureArgs } from './fixture-environment.mjs'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

// Exercise the production Electron renderer and preload subscription with
// recorded-shape snapshots. The isolated local demo supplies ordinary API
// metadata; no user's conversations, browser tabs or model credentials are used.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-conversation-e2e-'))
const artifacts = resolve('test-results')
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
const errors = []
let application
let page
let attachmentServer
const timestamp = (seconds) => new Date(Date.UTC(2026, 10, 1, 4, 0, seconds)).toISOString()
const message = (id, role, text, seconds, extra = {}) => ({
  id,
  role,
  text,
  status: 'complete',
  started_at: timestamp(seconds),
  completed_at: timestamp(seconds + 1),
  ...extra,
})
const tool = (id, name, args, result, seconds, extra = {}) =>
  message(id, 'tool', typeof result === 'string' ? result : JSON.stringify(result), seconds, {
    tool_name: name,
    tool_arguments: JSON.stringify(args),
    ...extra,
  })
const command = `node scripts/check.mjs --fixture=${'long-value-'.repeat(24)}`
const stdout =
  Array.from({ length: 100 }, (_, index) => `fixture output line ${index + 1}`).join('\n') +
  '\nEND_OF_COMPLETE_COMMAND_OUTPUT'

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
  const base = await page.evaluate(async () => {
    await window.harness.setPluginEnabled('builtin.llm-wiki', false)
    const created = await window.harness.createRun({
      prompt: '检查网页和脚本的执行结果',
      model_id: 'local-demo',
    })
    await window.harness.cancelRun(created.id)
    return window.harness.getRun(created.id)
  })
  const snapshot = {
    ...base,
    revision: 1_000_000,
    title: '检查网页和脚本的执行结果',
    status: 'completed',
    created_at: timestamp(0),
    updated_at: timestamp(620),
    context: null,
    error: undefined,
    pending_inputs: [],
    messages: [
      message('event-0', 'user', '检查网页和脚本的执行结果', 0),
      message('intro-1', 'assistant', '先查看网页，再核对脚本输出。', 1),
      tool(
        'open-1',
        'call_mcp_tool',
        {
          server_id: 'chrome',
          name: 'chrome_open',
          arguments: { url: 'https://example.com/news' },
        },
        { tab_id: 101, title: 'Fixture news' },
        2,
      ),
      tool(
        'read-1',
        'call_mcp_tool',
        {
          server_id: 'chrome',
          name: 'chrome_snapshot',
          arguments: { tab_id: 101 },
        },
        '页面暂时不可用，请重新读取。',
        4,
        { is_error: true },
      ),
      message('progress-1', 'assistant', '网页第一次读取失败，接下来核对本地脚本。', 8),
      tool(
        'command-1',
        'run_command',
        { command, timeout_ms: 30000 },
        { status: 'completed', exit_code: 0, stdout, stderr: '' },
        10,
      ),
      tool('file-1', 'read_file', { path: 'reports/result.md' }, '结果已核对。', 15),
      message('final-1', 'assistant', '第一轮已完成：脚本结果正常，网页需要重试。', 137),
      message('event-20', 'user', '再请协作任务独立检查一下', 600),
      message('intro-2', 'assistant', '请独立检查任务复核这些结果。', 601),
      tool(
        'agent-1',
        'spawn_agent',
        { task_name: '独立检查', prompt: '核对结果' },
        { agent_id: 'fixture-child', task_name: '独立检查' },
        602,
      ),
      tool(
        'agent-2',
        'agent_control',
        { action: 'result', target: '独立检查' },
        { status: 'completed', response: '检查通过。' },
        618,
      ),
      message('final-2', 'assistant', '复核已完成，两轮记录分别保留。', 619),
    ],
    events: [
      {
        sequence: 0,
        step: 0,
        type: 'run_started',
        label: '运行开始',
        detail: '',
        timestamp: timestamp(0),
      },
      {
        sequence: 19,
        step: 4,
        type: 'input_requested',
        label: '等待输入',
        detail: '',
        timestamp: timestamp(138),
      },
      {
        sequence: 20,
        step: 5,
        type: 'user_input',
        label: '收到输入',
        detail: '',
        timestamp: timestamp(600),
      },
      {
        sequence: 29,
        step: 7,
        type: 'run_completed',
        label: '已完成',
        detail: '',
        timestamp: timestamp(620),
      },
    ],
  }

  await application.evaluate(({ ipcMain }, fixture) => {
    globalThis.conversationFixture = fixture
    globalThis.conversationSubscription = undefined
    ipcMain.on('openharness:watch:start', (event, request) => {
      if (request.id !== globalThis.conversationFixture.id) return
      globalThis.conversationSubscription = { sender: event.sender, token: request.token }
      event.sender.send('openharness:watch:event', {
        token: request.token,
        snapshot: globalThis.conversationFixture,
      })
    })
  }, snapshot)
  const emit = async (next) =>
    application.evaluate((_electron, next) => {
      globalThis.conversationFixture = next
      const subscription = globalThis.conversationSubscription
      if (!subscription) throw new Error('No fixture run subscription')
      subscription.sender.send('openharness:watch:event', {
        token: subscription.token,
        snapshot: next,
      })
    }, next)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, base.id)
  const turns = page.locator('.assistant-turn')
  await expect(turns).toHaveCount(2)
  const first = turns.nth(0)
  const second = turns.nth(1)
  const work = first.locator('.turn-work')
  await expect(work).toHaveJSProperty('open', false)
  await expect(second.locator('.turn-work')).toHaveJSProperty('open', false)
  await expect(first.locator('.tool-activity')).toHaveCount(0)
  await expect(second.locator('.tool-activity')).toHaveCount(0)
  await expect(first.locator('.turn-answer')).toContainText('第一轮已完成')
  await expect(second.locator('.turn-answer')).toContainText('复核已完成')
  await expect(first.locator('.turn-answer')).toBeVisible()
  await expect(second.locator('.turn-answer')).toBeVisible()
  await expect(work.locator(':scope > summary')).toContainText('失败')
  await expect(work.locator(':scope > summary')).toContainText('2 分 18 秒')
  await expect(second.locator('.turn-work > summary')).toContainText('20 秒')
  await expect(page.locator('.tool-card')).toHaveCount(0)
  async function checkMessageAlignment() {
    const user = await page.locator('.message-user').first().boundingBox()
    const bubble = await page.locator('.message-user .message-body').first().boundingBox()
    const assistant = await page.locator('.message-assistant .message-body').first().boundingBox()
    assert.ok(
      Math.abs(bubble.x + bubble.width - user.x - user.width) <= 1,
      'User bubble is right-aligned',
    )
    assert.ok(bubble.x > assistant.x + 20, 'User and assistant have distinct alignment')
    assert.ok(Math.abs(assistant.x - user.x) <= 1, 'Assistant starts at the left conversation edge')
  }
  await checkMessageAlignment()
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-collapsed.png') })

  await work.locator(':scope > summary').click()
  await expect(first.getByText('先查看网页，再核对脚本输出。', { exact: true })).toBeVisible()
  await expect(first.locator('.tool-activity-group')).toHaveCount(2)
  for (const group of await first.locator('.tool-activity-group').all()) {
    if (!(await group.evaluate((element) => element.open)))
      await group.locator(':scope > summary').click()
  }
  const rows = first.locator('.tool-activity')
  await expect(rows).toHaveCount(4)
  await expect(rows.nth(0).locator(':scope > summary')).toContainText('打开网页')
  await expect(rows.nth(0).locator(':scope > summary')).toContainText('example.com/news')
  await expect(rows.nth(1).locator(':scope > summary')).toContainText('失败')
  assert.ok(
    !(await rows.locator(':scope > summary').allTextContents()).some((text) =>
      text.includes('call_mcp_tool'),
    ),
    'MCP transport names must not replace actual actions in compact titles',
  )
  for (const row of await rows.all()) {
    const summary = row.locator(':scope > summary')
    assert.ok((await summary.boundingBox()).height <= 44, 'Tool activity stays a compact row')
  }
  const firstTool = await rows.first().locator(':scope > summary').boundingBox()
  const firstProse = await first.locator('.message-rich-text').first().boundingBox()
  assert.ok(Math.abs(firstTool.x - firstProse.x) <= 1, 'Tool rows align with assistant prose')
  const ordered = await work.evaluate((element) =>
    [...element.querySelectorAll('.message-rich-text, .tool-activity > summary')].map(
      (node) => node.textContent,
    ),
  )
  const introduction = ordered.findIndex((text) => text.includes('先查看网页'))
  const open = ordered.findIndex((text) => text.includes('打开网页'))
  const progress = ordered.findIndex((text) => text.includes('接下来核对本地脚本'))
  const commandIndex = ordered.findIndex((text) => text.includes('运行命令'))
  assert.ok(
    introduction < open && open < progress && progress < commandIndex,
    'Prose remains interleaved with the corresponding tool operations',
  )
  const commandRow = rows.nth(2)
  await commandRow.locator(':scope > summary').click()
  await expect(commandRow).toContainText(command)
  await expect(commandRow).toContainText('END_OF_COMPLETE_COMMAND_OUTPUT')
  const completeResult = commandRow.locator('.activity-arguments').filter({
    has: page.locator('summary', { hasText: '完整结果' }),
  })
  await expect(completeResult).toHaveJSProperty('open', false)
  await completeResult.locator(':scope > summary').click()
  await expect(completeResult.locator('pre')).toContainText('"exit_code": 0')
  await expect(first.locator('.turn-answer')).toContainText('第一轮已完成')
  await expect(second.locator('.turn-answer')).not.toContainText('第一轮已完成')
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-expanded.png') })

  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 700),
  )
  await expect.poll(() => page.evaluate(() => innerWidth)).toBe(1000)
  await checkMessageAlignment()
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),
    false,
    'Long tool arguments and output must not widen the application',
  )
  assert.equal(
    await page
      .locator('.conversation-scroll')
      .evaluate((element) => element.scrollWidth > element.clientWidth),
    false,
    'Conversation must fit its narrow pane',
  )
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-compact.png') })

  const pending = tool(
    'live-open',
    'call_mcp_tool',
    {
      server_id: 'chrome',
      name: 'chrome_open',
      arguments: { url: 'https://example.com/retry' },
    },
    '',
    631,
    { status: 'pending', completed_at: undefined },
  )
  const running = {
    ...snapshot,
    revision: snapshot.revision + 1,
    status: 'running',
    messages: [...snapshot.messages, message('event-30', 'user', '现在重新读取网页', 630), pending],
    events: [
      ...snapshot.events,
      {
        sequence: 30,
        step: 8,
        type: 'user_input',
        label: '收到输入',
        detail: '',
        timestamp: timestamp(630),
      },
    ],
  }
  await emit(running)
  await expect(turns).toHaveCount(3)
  const third = turns.nth(2)
  await expect(third.locator('.turn-work')).toHaveJSProperty('open', true)
  await expect(third.locator('.tool-activity > summary')).toContainText('打开网页')
  await expect(third.locator('.tool-activity > summary')).toContainText('执行中')
  await expect(third.locator('.tool-activity > summary .animate-spin')).toHaveCount(1)
  await emit({
    ...running,
    revision: running.revision + 1,
    status: 'completed',
    messages: [
      ...running.messages.slice(0, -1),
      { ...pending, text: '页面已重新读取。', status: 'complete', completed_at: timestamp(635) },
      message('final-3', 'assistant', '重试完成，网页内容已读取。', 636),
    ],
    events: [
      ...running.events,
      {
        sequence: 39,
        step: 9,
        type: 'run_completed',
        label: '已完成',
        detail: '',
        timestamp: timestamp(637),
      },
    ],
  })
  // Settling a turn collapses and unmounts its work. Reopening must reconstruct
  // the settled row, without retaining the streaming invocation's spinner.
  await expect(third.locator('.turn-work')).toHaveJSProperty('open', false)
  await expect(third.locator('.tool-activity')).toHaveCount(0)
  await expect(third.locator('.turn-answer')).toContainText('重试完成')
  await expect(third.locator('.turn-answer')).toBeVisible()
  await third.locator('.turn-work > summary').click()
  await expect(third.locator('.tool-activity')).toHaveCount(1)
  await expect(third.locator('.tool-activity > summary')).toContainText('已打开网页')
  await expect(third.locator('.tool-activity .animate-spin')).toHaveCount(0)
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-settled.png') })

  // A pending invocation from an interrupted older turn cannot keep spinning.
  // Async command launches also settle once their later polling result arrives,
  // while the original recorded response remains inspectable without rewriting it.
  await emit({
    ...snapshot,
    revision: running.revision + 3,
    status: 'completed',
    messages: [
      message('event-40', 'user', '读取网页后先暂停', 650),
      message('interrupted-intro', 'assistant', '正在读取网页。', 651),
      { ...pending, id: 'interrupted-open', started_at: timestamp(652) },
      message('event-50', 'user', '现在运行异步检查脚本', 660),
      tool(
        'async-launch',
        'run_command',
        { command: 'node scripts/check.mjs' },
        { command_id: 'fixture-command', status: 'running', output: '检查已启动。' },
        661,
      ),
      tool(
        'async-result',
        'manage_command',
        { command_id: 'fixture-command', action: 'output' },
        {
          command_id: 'fixture-command',
          status: 'completed',
          output: 'ASYNC_CHECK_COMPLETE',
          exit_code: 0,
          next_cursor: 20,
          has_more_output: false,
        },
        668,
      ),
      message('async-final', 'assistant', '异步检查已经完成。', 669),
    ],
    events: [
      {
        sequence: 40,
        step: 0,
        type: 'user_input',
        label: '收到输入',
        detail: '',
        timestamp: timestamp(650),
      },
      {
        sequence: 49,
        step: 1,
        type: 'run_cancelled',
        label: '已停止',
        detail: '',
        timestamp: timestamp(653),
      },
      {
        sequence: 50,
        step: 2,
        type: 'user_input',
        label: '收到输入',
        detail: '',
        timestamp: timestamp(660),
      },
      {
        sequence: 59,
        step: 3,
        type: 'run_completed',
        label: '已完成',
        detail: '',
        timestamp: timestamp(670),
      },
    ],
  })
  await expect(turns).toHaveCount(2)
  const interrupted = turns.nth(0)
  const asynchronous = turns.nth(1)
  await expect(interrupted.locator('.turn-answer')).toHaveCount(0)
  await expect(interrupted.locator('.turn-work')).toHaveJSProperty('open', false)
  await expect(interrupted.locator('.tool-activity')).toHaveCount(0)
  await expect(interrupted.locator('.turn-work > summary')).toContainText('1 项失败')
  await interrupted.locator('.turn-work > summary').click()
  const interruptedRow = interrupted.locator('.tool-activity')
  await expect(interruptedRow.locator(':scope > summary')).toContainText('已中断')
  await expect(interrupted.locator('.animate-spin')).toHaveCount(0)
  await interruptedRow.locator(':scope > summary').click()
  await expect(interruptedRow).toContainText('结果尚未确认')
  await asynchronous.locator('.turn-work > summary').click()
  await asynchronous.locator('.tool-activity-group > summary').click()
  await expect(asynchronous.locator('.tool-activity')).toHaveCount(2)
  await expect(asynchronous.locator('.animate-spin')).toHaveCount(0)
  await expect(asynchronous.locator('.tool-activity > summary').first()).toContainText('已运行命令')
  const resultRow = asynchronous.locator('.tool-activity').nth(1)
  await resultRow.locator(':scope > summary').click()
  await expect(resultRow).toContainText('ASYNC_CHECK_COMPLETE')
  await resultRow.getByText('完整结果', { exact: true }).click()
  await expect(resultRow.locator('.activity-arguments[open] pre')).toContainText('"exit_code": 0')
  await expect(asynchronous.locator('.turn-answer')).toBeVisible()
  await expect(asynchronous.locator('.turn-answer')).toContainText('异步检查已经完成')
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-outcomes.png') })

  // A returned command snapshot describes the process at that moment. Neither
  // the launch nor a completed status check stays pending when no later poll is
  // recorded, even if their results still say the command was running.
  const pendingCommand = tool(
    'background-launch',
    'run_command',
    { command: 'node scripts/background-check.mjs' },
    '',
    681,
    { status: 'pending', completed_at: undefined },
  )
  const streamingStatus = tool(
    'background-status',
    'manage_command',
    { command_id: 'background-command', action: 'status' },
    '',
    682,
    { status: 'streaming', completed_at: undefined },
  )
  const commandCallsRunning = {
    ...snapshot,
    revision: running.revision + 4,
    status: 'running',
    messages: [
      message('event-60', 'user', '启动后台检查并查看命令状态', 680),
      pendingCommand,
      streamingStatus,
    ],
    events: [
      {
        sequence: 60,
        step: 0,
        type: 'user_input',
        label: '收到输入',
        detail: '',
        timestamp: timestamp(680),
      },
    ],
  }
  await emit(commandCallsRunning)
  await expect(turns).toHaveCount(1)
  const commandTurn = turns.first()
  const commandCalls = commandTurn.locator('.tool-activity')
  await expect(commandCalls).toHaveCount(2)
  await expect(commandTurn.locator('.turn-work')).toHaveJSProperty('open', true)
  await expect(commandTurn.locator('.tool-activity-group')).toHaveJSProperty('open', true)
  await expect(commandTurn.locator('.tool-activity-group > summary .animate-spin')).toHaveCount(1)
  for (const row of await commandCalls.all()) {
    await expect(row.locator(':scope > summary')).toContainText('执行中')
    await expect(row.locator(':scope > summary .animate-spin')).toHaveCount(1)
  }
  const backgroundResult = JSON.stringify({
    command_id: 'background-command',
    status: 'running',
    exit_code: null,
    output: '后台检查仍在运行。',
    next_cursor: 12,
    has_more_output: false,
    output_truncated: false,
  })
  await emit({
    ...commandCallsRunning,
    revision: commandCallsRunning.revision + 1,
    status: 'completed',
    messages: [
      commandCallsRunning.messages[0],
      {
        ...pendingCommand,
        status: 'complete',
        text: backgroundResult,
        completed_at: timestamp(684),
      },
      {
        ...streamingStatus,
        status: 'complete',
        text: backgroundResult,
        completed_at: timestamp(685),
      },
      message(
        'background-final',
        'assistant',
        '已启动后台检查并查询状态，查询返回时命令仍在运行。',
        686,
      ),
    ],
    events: [
      ...commandCallsRunning.events,
      {
        sequence: 69,
        step: 1,
        type: 'run_completed',
        label: '已完成',
        detail: '',
        timestamp: timestamp(687),
      },
    ],
  })
  await expect(commandTurn.locator('.turn-answer')).toBeVisible()
  await expect(commandTurn.locator('.turn-answer')).toContainText('已启动后台检查并查询状态')
  await expect(commandTurn.locator('.turn-work')).toHaveJSProperty('open', false)
  await expect(commandCalls).toHaveCount(0)
  await commandTurn.locator('.turn-work > summary').click()
  await expect(commandTurn.locator('.tool-activity-group')).toHaveJSProperty('open', false)
  await expect(commandCalls).toHaveCount(0)
  await commandTurn.locator('.tool-activity-group > summary').click()
  await expect(commandTurn.locator('.tool-activity-group > summary .animate-spin')).toHaveCount(0)
  await expect(commandCalls.nth(0).locator(':scope > summary .activity-label')).toHaveText(
    '已运行命令',
  )
  await expect(commandCalls.nth(1).locator(':scope > summary .activity-label')).toHaveText(
    '已检查命令状态',
  )
  for (const row of await commandCalls.all()) {
    await expect(row.locator(':scope > summary .activity-status')).toHaveText('返回时仍在运行')
    await expect(row.locator(':scope > summary .animate-spin')).toHaveCount(0)
  }
  await expect(commandTurn.locator('.animate-spin')).toHaveCount(0)
  await page.screenshot({ path: resolve(artifacts, 'conversation-activity-returned-commands.png') })

  // Use actual persisted attachments and the resource protocol for layout checks.
  // A loopback fixture replaces the model service; no external API is called.
  attachmentServer = createServer(async (request, response) => {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end('{"data":[]}')
      return
    }
    const input = JSON.parse(Buffer.concat(chunks).toString())
    const answer = { role: 'assistant', content: '图片和文件已收到，内容可以正常预览。' }
    if (!input.stream) {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(
        JSON.stringify({
          id: 'layout-response',
          choices: [{ index: 0, message: answer, finish_reason: 'stop' }],
          usage: { prompt_tokens: 20, completion_tokens: 10 },
        }),
      )
      return
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.end(
      `data: ${JSON.stringify({
        id: 'layout-response',
        choices: [
          {
            index: 0,
            delta: answer,
            finish_reason: 'stop',
          },
        ],
        usage: { prompt_tokens: 20, completion_tokens: 10 },
      })}\n\ndata: [DONE]\n\n`,
    )
  })
  await new Promise((resolve) => attachmentServer.listen(0, '127.0.0.1', resolve))
  const imageBase64 = await application.evaluate(({ nativeImage }) => {
    const pixels = Buffer.alloc(800 * 450 * 4)
    for (let offset = 0; offset < pixels.length; offset += 4) {
      pixels[offset] = 180
      pixels[offset + 1] = 205
      pixels[offset + 2] = 155
      pixels[offset + 3] = 255
    }
    return nativeImage
      .createFromBitmap(pixels, { width: 800, height: 450 })
      .toPNG()
      .toString('base64')
  })
  const attachmentRun = await page.evaluate(
    async ({ port, imageBase64 }) => {
      const model = await window.harness.configureModel({
        protocol: 'chat_completions',
        base_url: `http://127.0.0.1:${port}/v1`,
        api_key: 'layout-fixture',
        model: 'layout-fixture',
        supports_images: true,
        context_window: 64000,
        max_output_tokens: 2048,
        reasoning_mode: 'off',
        reasoning_effort: 'medium',
      })
      return window.harness.createRun({
        model_id: model.model.id,
        prompt: `检查图片和文件布局。\nhttps://example.com/${'long-unbroken-path-'.repeat(45)}\n这是一段需要自动换行的用户消息。`,
        files: [
          { name: '布局测试图片.png', image: { mime_type: 'image/png', data_base64: imageBase64 } },
          {
            name: `${'很长的文件名-'.repeat(18)}.md`,
            text: '# 文件布局验收\n本地生成的测试内容。',
          },
        ],
      })
    },
    { port: attachmentServer.address().port, imageBase64 },
  )
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, attachmentRun.id)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(2)
  await expect(page.locator('.message-user .file-reference-thumbnail')).toBeVisible()
  await expect
    .poll(() =>
      page
        .locator('.message-user .file-reference-thumbnail')
        .evaluate((image) => image.naturalWidth),
    )
    .toBe(800)
  await expect(page.locator('.message-assistant')).toContainText('图片和文件已收到')
  await checkMessageAlignment()
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false)
  const overflowingAttachments = await page.locator('.message-user').evaluate((element) => {
    const body = element.querySelector('.message-body').getBoundingClientRect()
    return [
      ...element.querySelectorAll('.message-text, .file-references, .file-references li, img'),
    ].filter((item) => {
      const bounds = item.getBoundingClientRect()
      return bounds.left < body.left - 1 || bounds.right > body.right + 1
    }).length
  })
  assert.equal(
    overflowingAttachments,
    0,
    'Long user text and real file/image references stay within the bubble',
  )
  await page.screenshot({
    path: resolve(artifacts, 'conversation-message-alignment-long-text.png'),
  })
  await page.locator('.conversation-scroll').evaluate((element) => {
    element.scrollTop = 0
  })
  await page.screenshot({
    path: resolve(artifacts, 'conversation-message-alignment-attachments.png'),
  })
  assert.deepEqual(errors, [])
  console.log(
    'PASS: compact semantic tool rows, full results, failure summaries, interleaved prose, isolated turns, live updates, interrupted requests, resolved async commands, user/agent alignment and narrow-window attachment layout',
  )
} catch (error) {
  await page
    ?.screenshot({ path: resolve(artifacts, 'conversation-activity-failure.png') })
    .catch(() => {})
  throw error
} finally {
  await application?.close()
  await new Promise((resolve) => (attachmentServer ? attachmentServer.close(resolve) : resolve()))
  await rm(directory, { recursive: true, force: true })
}
