import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

// Real Electron -> Rust runtime -> MCP process -> Chromium -> loopback website.
// The deterministic model fixture only chooses tools; no tool result or browser
// image is mocked, and no paid model endpoint or ordinary user profile is used.
const require = createRequire(import.meta.url)
const executable = process.env.OPENHARNESS_E2E_EXECUTABLE
if (executable && !isAbsolute(executable))
  throw new Error('OPENHARNESS_E2E_EXECUTABLE must be an absolute executable path')
const directory = await mkdtemp(join(tmpdir(), 'openharness-computer-e2e-'))
const screenshots = resolve('test-results')
await mkdir(screenshots, { recursive: true })
let application
let page
let runId
let browserServerId
let sequence = 0
let submitted = 0
let visits = 0
let heldResponse
let heldClosed = false
const modelRequests = []
const fixtureErrors = []
const pageErrors = []
const counts = new Map()
const firstPrompt = 'BROWSER_END_TO_END'
const secondPrompt = 'BROWSER_AFTER_REVOCATION'

const userPrompt = (request) =>
  request.messages
    .filter((message) => message.role === 'user' && typeof message.content === 'string')
    .at(-1)?.content
const tools = (request) => request.tools.map((tool) => tool.function.name)
const latestTool = (request) => {
  const content = request.messages.filter((message) => message.role === 'tool').at(-1)?.content
  assert.equal(typeof content, 'string', 'the runtime must return a tool result')
  const result = JSON.parse(content)
  assert(!result.error, `tool failed: ${content}`)
  if (result.content) return JSON.parse(result.content.find((block) => block.type === 'text').text)
  return result
}
const observation = (request) => {
  const result = latestTool(request)
  const snapshot = result.snapshot ?? result
  assert(
    Number.isInteger(snapshot.revision),
    'browser result must preserve structured snapshot/revision',
  )
  return snapshot
}
const callBrowser = (name, args = {}) => [
  'call_mcp_tool',
  { server_id: browserServerId, name, arguments: args },
]

const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET' && request.url?.startsWith('/page')) {
      visits += 1
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
      response.end(
        '<html><head><title>Open Harness local browser fixture</title></head><body><h1>Computer use acceptance fixture</h1><form method="post" action="/submit"><label for="email">Email</label><input id="email" name="email" type="email"><button>Save locally</button></form></body></html>',
      )
      return
    }
    if (request.method === 'POST' && request.url === '/submit') {
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      assert.equal(
        new URLSearchParams(Buffer.concat(chunks).toString()).get('email'),
        'browser@example.test',
      )
      submitted += 1
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
      response.end(
        `<html><head><title>Saved locally</title></head><body><h1>Verified local submission</h1><p>Submission count: ${submitted}</p><p>browser@example.test</p></body></html>`,
      )
      return
    }
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
      response.writeHead(404).end()
      return
    }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    modelRequests.push(body)
    const prompt = userPrompt(body)
    const count = counts.get(prompt) ?? 0
    counts.set(prompt, count + 1)
    let call
    if (prompt === firstPrompt) {
      switch (count) {
        case 0:
          call = ['list_mcp_servers', {}]
          break
        case 1: {
          const servers = latestTool(body).servers
          assert(
            servers.some((entry) => entry.id === browserServerId),
            'installed browser must be discoverable',
          )
          call = ['list_mcp_tools', { server_id: browserServerId }]
          break
        }
        case 2:
          assert(latestTool(body).tools.some((tool) => tool.name === 'browser_screenshot'))
          call = ['load_skill', { id: 'browser-use:browser-use' }]
          break
        case 3:
          call = callBrowser('browser_open', { url: `${origin}/page` })
          break
        case 4:
          assert.match(observation(body).text, /Computer use acceptance fixture/)
          call = callBrowser('browser_snapshot')
          break
        case 5: {
          const snapshot = observation(body)
          const email = snapshot.elements.find((element) => element.name === 'Email')
          assert(email, 'the real page must expose its email field')
          call = callBrowser('browser_action', {
            action: 'fill',
            revision: snapshot.revision,
            ref: email.ref,
            text: 'browser@example.test',
          })
          break
        }
        case 6: {
          const snapshot = observation(body)
          const button = snapshot.elements.find((element) => element.name === 'Save locally')
          assert(button)
          call = callBrowser('browser_action', {
            action: 'click',
            revision: snapshot.revision,
            ref: button.ref,
          })
          break
        }
        case 7:
          assert.match(observation(body).text, /Verified local submission/)
          assert.equal(submitted, 1, 'the form must have been submitted exactly once')
          call = callBrowser('browser_screenshot')
          break
        case 8: {
          const images = body.messages
            .flatMap((message) => (Array.isArray(message.content) ? message.content : []))
            .filter((block) => block.type === 'image_url')
          assert.equal(
            images.length,
            1,
            'the real MCP screenshot must reach the next model request',
          )
          assert.match(images[0].image_url.url, /^data:image\/jpeg;base64,/)
          assert(Buffer.from(images[0].image_url.url.split(',')[1], 'base64').length > 1000)
          break
        }
        default:
          throw new Error(`Unexpected first-run model step ${count}`)
      }
    } else if (prompt === secondPrompt) {
      if (count === 0) call = callBrowser('browser_open', { url: `${origin}/page?resume=1` })
      else if (count === 1) call = callBrowser('browser_screenshot')
      else if (count !== 2) throw new Error(`Unexpected resumed model step ${count}`)
    } else throw new Error(`Unexpected prompt ${prompt}`)
    if (call) assert(tools(body).includes(call[0]), `${call[0]} must be advertised before use`)
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const id = ++sequence
    const delta = call
      ? {
          role: 'assistant',
          tool_calls: [
            {
              index: 0,
              id: `computer-tool-${id}`,
              type: 'function',
              function: { name: call[0], arguments: JSON.stringify(call[1]) },
            },
          ],
        }
      : {
          role: 'assistant',
          content:
            prompt === firstPrompt
              ? '浏览器端到端验证完成。'
              : '截图已收到，正在等待停止控制校验。',
        }
    response.write(
      `data: ${JSON.stringify({ id: `computer-response-${id}`, choices: [{ index: 0, delta, finish_reason: call ? 'tool_calls' : prompt === firstPrompt ? 'stop' : null }] })}\n\n`,
    )
    if (prompt === secondPrompt && count === 2) {
      heldResponse = response
      response.on('close', () => {
        heldClosed = true
      })
    } else response.end('data: [DONE]\n\n')
  } catch (error) {
    fixtureErrors.push(error.stack ?? String(error))
    if (!response.headersSent) response.writeHead(500)
    response.end('Local test fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const origin = `http://127.0.0.1:${server.address().port}`
const env = {
  ...process.env,
  OPENHARNESS_DESKTOP_DATA_DIR: join(directory, 'data'),
  PLAYWRIGHT_BROWSERS_PATH:
    process.env.PLAYWRIGHT_BROWSERS_PATH ?? '/private/tmp/openharness-playwright-browsers',
}
delete env.ELECTRON_RUN_AS_NODE
const runExtensions = (page, id) => page.evaluate((id) => window.harness.runExtensions(id), id)
const detail = (page, id) => page.evaluate((id) => window.harness.getRun(id), id)
try {
  application = await _electron.launch({
    executablePath: executable ?? require('electron'),
    args: executable ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  const applicationInfo = await application.evaluate(({ app }) => ({
    packaged: app.isPackaged,
    executable: process.execPath,
    resourcesPath: process.resourcesPath,
    platform: process.platform,
  }))
  if (executable)
    assert.equal(
      applicationInfo.packaged,
      true,
      'explicit executable must exercise the packaged app',
    )
  page = await application.firstWindow()
  page.on('pageerror', (error) => pageErrors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  await page.getByRole('link', { name: '插件', exact: true }).click()
  const builtIn = page.getByRole('article', { name: '内置能力 浏览器操作', exact: true })
  await expect(builtIn).toBeVisible()
  await expect(builtIn.getByRole('button', { name: '安装', exact: true })).toBeEnabled()
  assert.equal(
    (await page.evaluate(() => window.harness.bundledPlugins())).find(
      (plugin) => plugin.id === 'browser-use',
    ).browserReady,
    true,
  )
  await builtIn.getByRole('button', { name: '安装', exact: true }).click()
  await expect(page.getByRole('article', { name: '插件 Browser Use', exact: true })).toBeVisible()
  const settings = await page.evaluate(() => window.harness.extensionSettings())
  browserServerId = settings.plugins.find((plugin) => plugin.id === 'browser-use').mcp_servers[0].id
  assert(settings.skills.some((skill) => skill.id === 'browser-use:browser-use'))
  assert.equal(visits, 0, 'installing a plugin must not open a browser page')
  const configured = await page.evaluate((input) => window.harness.configureModel(input), {
    protocol: 'chat_completions',
    base_url: `${origin}/v1`,
    api_key: 'local-fixture-only',
    model: 'computer-use-fixture',
    context_window: 128000,
    max_output_tokens: 2048,
    reasoning_mode: 'off',
    reasoning_effort: 'medium',
    supports_images: true,
  })
  await page.getByRole('link', { name: '会话', exact: true }).click()
  const run = await page.evaluate(
    ({ prompt, model }) => window.harness.createRun({ prompt, model_id: model }),
    { prompt: firstPrompt, model: configured.model.id },
  )
  runId = run.id
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, run.id)
  await expect
    .poll(async () => (await runExtensions(page, run.id)).approvals.length, { timeout: 30_000 })
    .toBe(1)
  assert.equal(visits, 0, 'navigation must await user authorization')
  const pending = (await runExtensions(page, run.id)).approvals[0]
  assert.deepEqual(pending.session_scope, { kind: 'browser' })
  await expect(page.getByRole('button', { name: '允许本会话', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '允许本会话', exact: true }).click()
  await expect
    .poll(async () => (await detail(page, run.id)).status, { timeout: 45_000 })
    .toBe('waiting_for_input')
  const finished = await detail(page, run.id)
  assert.deepEqual(fixtureErrors, [])
  assert.equal(submitted, 1)
  assert(
    finished.messages.some((message) => message.images?.length === 1),
    'screenshot must be persisted in conversation projection',
  )
  assert.equal((await runExtensions(page, run.id)).controls.length, 1)
  assert.equal(
    (await runExtensions(page, run.id)).approvals.length,
    0,
    'the grant covers subsequent approved browser operations',
  )
  const screenshotCard = page.locator('.tool-card').last()
  await expect(screenshotCard.locator('.message-image-button')).toHaveCount(0)
  await screenshotCard.locator('summary').click()
  const preview = screenshotCard.locator('.message-image-button')
  await expect(preview).toBeVisible()
  await preview.click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.locator('img')).toBeVisible()
  await expect.poll(() => dialog.locator('img').evaluate((image) => image.naturalWidth)).toBe(1280)
  await page.screenshot({ path: join(screenshots, 'desktop-browser-screenshot-preview.png') })
  await page.getByRole('button', { name: '关闭预览', exact: true }).click()
  await expect(page.getByRole('button', { name: '停止控制', exact: true })).toBeVisible()
  await page.screenshot({ path: join(screenshots, 'desktop-browser-session-grant.png') })
  await page.getByRole('button', { name: '停止控制', exact: true }).click()
  await expect.poll(async () => (await runExtensions(page, run.id)).controls.length).toBe(0)

  const visitsBeforeResume = visits
  await page.evaluate(({ id, message }) => window.harness.resumeRun(id, message), {
    id: run.id,
    message: secondPrompt,
  })
  await expect.poll(async () => (await runExtensions(page, run.id)).approvals.length).toBe(1)
  assert.equal(
    visits,
    visitsBeforeResume,
    'revocation must prevent automatic browser navigation in the next turn',
  )
  await page.getByRole('button', { name: '允许本会话', exact: true }).click()
  await expect.poll(() => Boolean(heldResponse), { timeout: 30_000 }).toBe(true)
  assert.equal((await detail(page, run.id)).status, 'running')
  await page.getByRole('button', { name: '停止控制', exact: true }).click()
  await expect.poll(async () => (await detail(page, run.id)).status).toBe('cancelled')
  await expect.poll(async () => (await runExtensions(page, run.id)).controls.length).toBe(0)
  await expect.poll(() => heldClosed).toBe(true)
  assert.equal((await runExtensions(page, run.id)).approvals.length, 0)
  assert.equal(submitted, 1, 'cancellation must not replay an earlier form submission')
  assert.deepEqual(fixtureErrors, [])
  assert.deepEqual(pageErrors, [])
  // This goes through the running app's main-process BundledPlugins bridge.
  // macOS permissions belong to that launched application/helper attribution;
  // a status result from directly executing the helper in a shell is different.
  const computer = (await page.evaluate(() => window.harness.bundledPlugins())).find(
    (plugin) => plugin.id === 'computer-use',
  )
  if (applicationInfo.platform === 'darwin') {
    assert.equal(computer.available, true, 'the app must contain its native helper')
    assert.equal(typeof computer.permissions?.accessibility, 'boolean')
    assert.equal(typeof computer.permissions?.screenRecording, 'boolean')
    if (applicationInfo.packaged)
      assert(computer.permissions.helperPath.startsWith(applicationInfo.resourcesPath))
  }
  const report = {
    passed: true,
    directory,
    application: applicationInfo,
    computerPermissionsFromApp: computer.permissions ?? null,
    modelRequests: modelRequests.length,
    submissions: submitted,
    checks: [
      'visible first-party plugin installation',
      'lazy server/tool/skill discovery',
      'user session authorization',
      'real isolated Chromium navigation and form submission',
      'actual MCP JPEG in next model request',
      'persisted screenshot preview in Electron',
      'revocation blocks automatic continuation',
      'stop control cancels an active run and clears authorization',
    ],
  }
  await writeFile(
    join(
      screenshots,
      applicationInfo.packaged ? 'computer-use-packaged-report.json' : 'computer-use-report.json',
    ),
    JSON.stringify(report, null, 2),
  )
  console.log(JSON.stringify(report, null, 2))
} catch (error) {
  console.error(
    JSON.stringify(
      { directory, fixtureErrors, modelSteps: [...counts], visits, submitted },
      null,
      2,
    ),
  )
  if (page && !page.isClosed()) {
    await page
      .screenshot({ path: join(screenshots, 'desktop-browser-failure.png') })
      .catch(() => {})
  }
  throw error
} finally {
  heldResponse?.end()
  try {
    // Assertions can fail before the UI's stop-control step. Explicitly await
    // runtime cleanup before closing the app that owns the MCP transport.
    if (runId && page && !page.isClosed())
      await page.evaluate((id) => window.harness.revokeControl(id), runId).catch(() => {})
  } finally {
    try {
      await application?.close()
    } finally {
      server.closeAllConnections()
      await new Promise((resolve) => server.close(resolve))
    }
  }
}
