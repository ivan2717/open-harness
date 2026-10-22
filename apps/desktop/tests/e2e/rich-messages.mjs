import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, realpath, unlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-rich-e2e-'))
const dataDirectory = join(directory, 'app-data')
const project = join(directory, 'project')
const artifacts = resolve('test-results')
await Promise.all([
  mkdir(join(project, 'assets'), { recursive: true }),
  mkdir(artifacts, { recursive: true }),
])
const root = await realpath(project)
const inputPath = join(directory, '输入 snapshot.md')
const imagePath = join(root, 'assets', 'image sample.png')
const svgPath = join(root, 'assets', 'vector.svg')
const audioPath = join(root, 'assets', 'tone sample.wav')
const videoPath = join(root, 'assets', 'clip sample.webm')
const reportPath = join(root, 'report sample.md')
const savePath = join(directory, 'saved-report.md')
const inputText = '# INPUT_SNAPSHOT\nOriginal user file, retained after its source is deleted.\n'
const reportText = '# GENERATED_REPORT\nThis is the original output snapshot.\n'
const codeText = 'const sample = "RICH_CODE_COPY";\nconsole.log(sample);'
const userText = '**USER_LITERAL** <b>用户原文</b>\n请展示富文本和输出文件。'
const imageBytes = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq1sAAAAASUVORK5CYII=',
  'base64',
)
// Tiny 32×32 VP8 WebM fixture generated once with ffmpeg; no ffmpeg runtime dependency.
const videoBytes = Buffer.from(
  'GkXfo59ChoEBQveBAULygQRC84EIQoKEd2VibUKHgQJChYECGFOAZwEAAAAAAAIaEU2bdLpNu4tTq4QVSalmU6yBoU27i1OrhBZUrmtTrIHYTbuMU6uEElTDZ1OsggElTbuMU6uEHFO7a1OsggIE7AEAAAAAAABZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVSalmsirXsYMPQkBNgI1MYXZmNjIuMTIuMTAwV0GNTGF2ZjYyLjEyLjEwMESJiECPQAAAAAAAFlSua8iuAQAAAAAAAD/XgQFzxYgaz2UB9RO4wZyBACK1nIN1bmSIgQCGhVZfVlA4g4EBI+ODhB3NZQDgkLCBILqBIJqBAlWwhFW5gQESVMNn/HNzoGPAgGfImkWjh0VOQ09ERVJEh41MYXZmNjIuMTIuMTAwc3PWY8CLY8WIGs9lAfUTuMFnyKFFo4dFTkNPREVSRIeUTGF2YzYyLjI4LjEwMCBsaWJ2cHhnyKFFo4hEVVJBVElPTkSHkzAwOjAwOjAxLjAwMDAwMDAwMAAfQ7Z12eeBAKO9gQAAgBADAJ0BKiAAIAAARwiFhYiFhIgCAgJ1qgP4A/oCCFkMvQD+/W7z/+K1h1nF/4rf/9Nk8VeM/+mLAKOVgQH0ALEBAAUQrAAYABhYL/QACHAAHFO7a5G7j7OBALeK94EB8YIBpvCBAw==',
  'base64',
)
const audioBytes = Buffer.alloc(44 + 8000 * 2)
audioBytes.write('RIFF', 0)
audioBytes.writeUInt32LE(audioBytes.length - 8, 4)
audioBytes.write('WAVEfmt ', 8)
audioBytes.writeUInt32LE(16, 16)
audioBytes.writeUInt16LE(1, 20)
audioBytes.writeUInt16LE(1, 22)
audioBytes.writeUInt32LE(8000, 24)
audioBytes.writeUInt32LE(16000, 28)
audioBytes.writeUInt16LE(2, 32)
audioBytes.writeUInt16LE(16, 34)
audioBytes.write('data', 36)
audioBytes.writeUInt32LE(audioBytes.length - 44, 40)
for (let i = 0; i < 8000; i++)
  audioBytes.writeInt16LE(Math.sin((i * Math.PI * 2 * 440) / 8000) * 1000, 44 + i * 2)
await Promise.all([
  writeFile(inputPath, inputText),
  writeFile(imagePath, imageBytes),
  writeFile(audioPath, audioBytes),
  writeFile(videoPath, videoBytes),
  writeFile(reportPath, reportText),
  writeFile(join(directory, 'outside.txt'), 'OUTSIDE_WORKSPACE_SECRET'),
])

const partial = '## 富文本验收\n\n**加粗文字**和普通段落。\n\n```typescript\nconst sample = '
let remainder =
  '"RICH_CODE_COPY";\nconsole.log(sample);\n```\n\n' +
  '| 类型 | 状态 |\n| --- | --- |\n| Markdown | 已渲染 |\n\n' +
  '> 引用内容\n\n- 列表一\n- 列表二\n\n' +
  '![图片预览](<./assets/image sample.png>)\n\n' +
  '![矢量预览](./assets/vector.svg)\n\n' +
  '[音频预览](<./assets/tone sample.wav>)\n\n' +
  '[视频预览](<./assets/clip sample.webm>)\n\n' +
  '[生成报告](<./report sample.md>)\n\n' +
  '[缺失文件](./missing.txt)\n\n[越界文件](../outside.txt)\n\n' +
  '[外部网页](https://example.com/rich-test)\n\n' +
  '[危险链接](javascript:alert%281%29)\n\n' +
  '<img src="unsafe-image" onerror="window.__richUnsafe = true">\n\n' +
  '<script>window.__richUnsafe = true</script>\n'
const calls = []
const fixtureErrors = []
const errors = []
let remoteImageRequests = 0
let svgBeaconRequests = 0
let finishReply
const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      if (request.url === '/svg-beacon') svgBeaconRequests += 1
      if (request.url === '/remote-image.png') {
        remoteImageRequests += 1
        response.writeHead(200, { 'Content-Type': 'image/png' })
        response.end(imageBytes)
        return
      }
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(JSON.stringify({ data: [] }))
      return
    }
    assert.equal(request.method, 'POST')
    assert.equal(request.url, '/v1/chat/completions')
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    calls.push(JSON.parse(Buffer.concat(chunks).toString()))
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    const send = (delta, finish_reason = null) =>
      response.write(
        `data: ${JSON.stringify({
          id: 'rich-response',
          choices: [{ index: 0, delta, finish_reason }],
        })}\n\n`,
      )
    send({ role: 'assistant', content: partial })
    finishReply = () => {
      send({ content: remainder }, 'stop')
      response.write(
        `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 700, completion_tokens: 350 } })}\n\n`,
      )
      response.end('data: [DONE]\n\n')
      finishReply = undefined
    }
  } catch (error) {
    fixtureErrors.push(error.message)
    response.writeHead(500)
    response.end('Fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
remainder += `\n![远程图片](http://127.0.0.1:${server.address().port}/remote-image.png)\n`
const svgBeacon = `http://127.0.0.1:${server.address().port}/svg-beacon`
await writeFile(
  svgPath,
  `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><rect width="120" height="80" fill="#66856a"/><script>fetch('${svgBeacon}')</script><image href="${svgBeacon}" width="1" height="1"/></svg>`,
)
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  api_key: 'rich-message-fixture-key',
  model: 'rich-message-fixture',
  context_window: 64000,
  max_output_tokens: 2048,
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
  await application.evaluate(
    ({ dialog, shell, clipboard }, { root, inputPath, savePath }) => {
      globalThis.__richActions = []
      clipboard.writeText = (text) => {
        globalThis.__richCopied = text
      }
      dialog.showOpenDialog = async (_window, options) => ({
        canceled: false,
        filePaths: [options.properties.includes('openDirectory') ? root : inputPath],
      })
      dialog.showSaveDialog = async () => ({ canceled: false, filePath: savePath })
      shell.openPath = async (path) => {
        globalThis.__richActions.push(['open', path])
        return ''
      }
      shell.showItemInFolder = (path) => {
        globalThis.__richActions.push(['reveal', path])
      }
      shell.openExternal = async (url) => {
        globalThis.__richActions.push(['external', url])
      }
    },
    { root, inputPath, savePath },
  )
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await page.reload()
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await page.evaluate(() => window.harness.config())
  return page
}

async function closeApplication() {
  await application?.close()
  application = undefined
}

async function resource(page, request) {
  return page.evaluate((request) => window.harness.resolveResource(request), request)
}

async function assertPlayable(locator) {
  await expect(locator).toBeVisible()
  await locator.evaluate((element) => {
    element.muted = true
    element.load()
  })
  await expect
    .poll(() => locator.evaluate((element) => element.readyState), { timeout: 15_000 })
    .toBeGreaterThanOrEqual(2)
  await locator.evaluate((element) => element.play())
  await expect
    .poll(() => locator.evaluate((element) => element.currentTime), { timeout: 5000 })
    .toBeGreaterThan(0)
  await locator.evaluate((element) => element.pause())
}

try {
  let page = await launch()
  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    configuration,
  )
  await page.evaluate((selectedModel) => {
    localStorage.setItem(
      'open-harness-workspace',
      JSON.stringify({
        state: { drafts: {}, selectedModel, inspectorOpen: false, inspectorTab: 'context' },
        version: 1,
      }),
    )
  }, configured.model.id)
  await page.reload()
  await page.getByRole('button', { name: '选择项目', exact: true }).click()
  await page.getByRole('button', { name: '添加文件', exact: true }).click()
  await expect(page.locator('.composer .file-references')).toContainText('输入 snapshot.md')
  await page.getByRole('textbox', { name: '任务描述', exact: true }).fill(userText)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const runId = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  await expect(page.locator('.message-assistant .message-markdown h2')).toHaveText('富文本验收')
  await expect(page.locator('.message-assistant .message-code-block')).toContainText(
    'const sample =',
  )
  await expect(page.locator('.message-live')).toBeVisible()
  await expect(page.locator('.message-user .message-text')).toHaveText(userText)
  await expect(page.locator('.message-user strong, .message-user b')).toHaveCount(0)
  assert.equal(typeof finishReply, 'function', 'fixture waits for the streaming UI assertion')
  finishReply()
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, runId), {
      timeout: 20_000,
    })
    .toBe('waiting_for_input')
  const detail = await page.evaluate((id) => window.harness.getRun(id), runId)
  const assistant = detail.messages.find((message) => message.role === 'assistant')
  const user = detail.messages.find((message) => message.role === 'user')
  assert.equal(
    assistant.text,
    partial + remainder,
    'presentation must preserve the original Markdown source',
  )
  const scope = { runId, messageId: assistant.id }
  await expect(page.locator('.message-markdown strong')).toHaveText('加粗文字')
  await expect(page.locator('.message-markdown table')).toContainText('已渲染')
  await expect(page.locator('.message-markdown blockquote')).toContainText('引用内容')
  await page.getByRole('button', { name: '复制代码', exact: true }).click()
  await expect
    .poll(() => application.evaluate(() => globalThis.__richCopied?.trimEnd()))
    .toBe(codeText)
  await assert.rejects(
    page.evaluate(() => window.harness.copyText('x'.repeat(2 * 1024 * 1024 + 1))),
  )
  assert.equal(await page.evaluate(() => Boolean(window.__richUnsafe)), false)
  await expect(
    page.locator(
      '.message-markdown script, .message-markdown img[src="unsafe-image"], .message-markdown a[href^="javascript:"]',
    ),
  ).toHaveCount(0)

  const image = page.locator('.message-resource-image img').first()
  await image.scrollIntoViewIfNeeded()
  await expect
    .poll(() => image.evaluate((element) => element.naturalWidth), { timeout: 10_000 })
    .toBe(1)
  await page.getByRole('button', { name: '放大查看 image sample.png', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'image sample.png', exact: true })).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'rich-message-preview.png') })
  await page.getByRole('button', { name: '关闭预览', exact: true }).click()
  const svgImage = page.getByRole('img', { name: 'vector.svg', exact: true })
  await svgImage.scrollIntoViewIfNeeded()
  await expect.poll(() => svgImage.evaluate((image) => image.naturalWidth)).toBe(120)
  assert.equal(
    svgBeaconRequests,
    0,
    'SVG images cannot run scripts or load nested remote resources',
  )
  await assertPlayable(page.locator('.message-resource-audio audio'))
  await assertPlayable(page.locator('.message-resource-video video'))
  assert.equal(remoteImageRequests, 0, 'remote media must wait for an explicit load action')
  await page.getByRole('button', { name: /加载图片/ }).click()
  await expect.poll(() => remoteImageRequests).toBe(1)
  await expect
    .poll(() =>
      page
        .getByRole('img', { name: '远程图片', exact: true })
        .evaluate((element) => element.naturalWidth),
    )
    .toBe(1)

  const videoResource = await resource(page, { ...scope, source: './assets/clip sample.webm' })
  // Renderer CSP intentionally disallows fetch; media elements use the scheme
  // directly. Exercise its explicit byte-range response through Electron net.
  const range = await application.evaluate(async ({ net }, url) => {
    const response = await net.fetch(url, { headers: { Range: 'bytes=0-15' } })
    return {
      status: response.status,
      range: response.headers.get('content-range'),
      bytes: Array.from(new Uint8Array(await response.arrayBuffer())),
    }
  }, videoResource.url)
  assert.equal(range.status, 206)
  assert.equal(range.range, `bytes 0-15/${videoBytes.length}`)
  assert.deepEqual(range.bytes, Array.from(videoBytes.subarray(0, 16)))

  const reportCard = page.locator('.message-resource-text').filter({ hasText: '生成报告' })
  await reportCard.locator('.message-resource-name').click()
  await expect(page.getByRole('dialog', { name: 'report sample.md', exact: true })).toContainText(
    'GENERATED_REPORT',
  )
  await page.getByRole('button', { name: '关闭预览', exact: true }).click()
  await reportCard.getByRole('button', { name: '另存为', exact: true }).click()
  await expect.poll(() => readFile(savePath, 'utf8').catch(() => '')).toBe(reportText)
  await reportCard.getByRole('button', { name: '打开', exact: true }).click()
  await reportCard.getByRole('button', { name: '显示位置', exact: true }).click()
  await page.getByRole('link', { name: '外部网页', exact: true }).click()
  await expect
    .poll(() => application.evaluate(() => globalThis.__richActions.map(([action]) => action)))
    .toEqual(['open', 'reveal', 'external'])

  const inputResource = await resource(page, { runId, messageId: user.id, fileIndex: 0 })
  assert.equal(inputResource.text, inputText)
  await page.getByRole('button', { name: '预览文件 输入 snapshot.md', exact: true }).click()
  await expect(page.getByRole('dialog', { name: '输入 snapshot.md', exact: true })).toContainText(
    'INPUT_SNAPSHOT',
  )
  await page.getByRole('button', { name: '关闭预览', exact: true }).click()
  await assert.rejects(resource(page, { ...scope, source: '../outside.txt' }))
  await assert.rejects(resource(page, { ...scope, source: './missing.txt' }))
  await assert.rejects(resource(page, { ...scope, source: 'javascript:alert(1)' }))
  await assert.rejects(resource(page, { ...scope, source: './not-in-message.txt' }))
  await assert.rejects(page.evaluate(() => window.harness.openExternal('javascript:alert(1)')))
  assert(!JSON.stringify(detail).includes('OUTSIDE_WORKSPACE_SECRET'))
  assert(
    !(await page
      .locator('body')
      .textContent()
      .then((text) => text.includes('OUTSIDE_WORKSPACE_SECRET'))),
  )
  await page.locator('.conversation-scroll').evaluate((element) => {
    element.scrollTop = 0
  })
  await page.screenshot({ path: join(artifacts, 'rich-messages.png') })

  const reportResource = await resource(page, { ...scope, source: './report sample.md' })
  const imageResource = await resource(page, { ...scope, source: './assets/image sample.png' })
  const audioResource = await resource(page, { ...scope, source: './assets/tone sample.wav' })
  await closeApplication()
  await Promise.all(
    [inputPath, imagePath, svgPath, audioPath, videoPath, reportPath].map((path) => unlink(path)),
  )
  page = await launch()
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, runId)
  await expect(page.locator('.message-markdown h2')).toHaveText('富文本验收')
  for (const [source, previous] of [
    ['./report sample.md', reportResource],
    ['./assets/image sample.png', imageResource],
    ['./assets/tone sample.wav', audioResource],
    ['./assets/clip sample.webm', videoResource],
  ]) {
    const restored = await resource(page, { ...scope, source })
    assert.equal(restored.id, previous.id, 'restart must reuse the saved snapshot')
    assert.equal(restored.size, previous.size)
  }
  assert.equal((await resource(page, { ...scope, source: './report sample.md' })).text, reportText)
  assert.equal((await resource(page, { runId, messageId: user.id, fileIndex: 0 })).text, inputText)
  await page.locator('.message-resource-image img').first().scrollIntoViewIfNeeded()
  await expect
    .poll(
      () =>
        page
          .locator('.message-resource-image img')
          .first()
          .evaluate((element) => element.naturalWidth),
      { timeout: 10_000 },
    )
    .toBe(1)
  await assertPlayable(page.locator('.message-resource-video video'))
  assert.equal(
    calls.length,
    1,
    'viewing and restoring files must not request another model response',
  )
  assert.deepEqual(fixtureErrors, [])
  assert.deepEqual(errors, [])
  assert.equal(svgBeaconRequests, 0)
  console.log(
    `Rich message E2E passed: streamed GFM, copy, image/media, file actions, access checks, and restart snapshots (${directory}).`,
  )
} finally {
  finishReply?.()
  await closeApplication()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
