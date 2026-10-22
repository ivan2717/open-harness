import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-file-e2e-'))
const dataDirectory = join(directory, 'app-data')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const firstPath = join(directory, '计划 sample.ts')
const secondPath = join(directory, 'clipboard-notes.md')
const thirdPath = join(directory, 'resume.txt')
const imagePath = join(directory, 'unsupported.png')
const largePath = join(directory, 'too-large.txt')
const firstText = 'export const plan = "CREATE_FILE_VERSION_1";\n// 发送时保留这个快照。\n'
const secondText = '# PASTE_FILE_SNAPSHOT\nThese notes came from the native file clipboard.\n'
const thirdText = 'RESUME_FILE_SNAPSHOT\nContinue from the recorded state.\n'
await Promise.all([
  writeFile(firstPath, firstText),
  writeFile(secondPath, secondText),
  writeFile(thirdPath, thirdText),
  writeFile(imagePath, Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])),
  writeFile(largePath, 'x'.repeat(256 * 1024 + 1)),
])

const calls = []
const errors = []
const fixtureErrors = []
let holdNext = false
const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(JSON.stringify({ data: [] }))
      return
    }
    assert.equal(request.method, 'POST')
    assert.equal(request.url, '/v1/chat/completions')
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    if (holdNext) {
      holdNext = false
      return
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.write(
      `data: ${JSON.stringify({
        id: `reference-${calls.length}`,
        choices: [
          {
            index: 0,
            delta: { role: 'assistant', content: 'The referenced snapshot was received.' },
            finish_reason: 'stop',
          },
        ],
      })}\n\n`,
    )
    response.write(
      `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 500, completion_tokens: 20 } })}\n\n`,
    )
    response.end('data: [DONE]\n\n')
  } catch (error) {
    fixtureErrors.push(error.message)
    response.writeHead(500)
    response.end('Fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  model: 'file-reference-fixture',
  api_key: 'file-reference-fixture-key',
  context_window: 64000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: dataDirectory }
delete env.ELECTRON_RUN_AS_NODE
let application
let clipboardBackedUp = false

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
  await page.evaluate(() => window.harness.config())
  return page
}

async function clipboardBackup() {
  await application.evaluate(async ({ clipboard, ClipboardItem }) => {
    // read() returns lazy handles. Materialize every payload before changing
    // the OS clipboard; nothing from the user's clipboard leaves this process.
    const backup = []
    for (const item of await clipboard.read()) {
      const values = {}
      for (const type of item.types) values[type] = await item.getType(type)
      backup.push(new ClipboardItem(values))
    }
    globalThis.__fileReferenceClipboardBackup = backup
  })
  clipboardBackedUp = true
}

async function restoreClipboard() {
  if (!application || !clipboardBackedUp) return
  await application.evaluate(async ({ clipboard }) => {
    await clipboard.write(globalThis.__fileReferenceClipboardBackup)
    delete globalThis.__fileReferenceClipboardBackup
  })
  clipboardBackedUp = false
}

async function closeApplication() {
  await restoreClipboard()
  await application?.close()
  application = undefined
}

async function diskFileEvent(page, paths, kind = 'drop') {
  await page.evaluate(() => {
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    input.hidden = true
    input.id = 'file-reference-test-input'
    document.body.append(input)
  })
  const input = page.locator('#file-reference-test-input')
  await input.setInputFiles(paths)
  await input.evaluate((input, kind) => {
    const data = new DataTransfer()
    for (const file of input.files) data.items.add(file)
    if (kind === 'drop') {
      const composer = document.querySelector('.composer')
      composer.dispatchEvent(
        new DragEvent('dragenter', { dataTransfer: data, bubbles: true, cancelable: true }),
      )
      composer.dispatchEvent(
        new DragEvent('drop', { dataTransfer: data, bubbles: true, cancelable: true }),
      )
    } else {
      document
        .querySelector('.composer textarea')
        .dispatchEvent(
          new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }),
        )
    }
    input.remove()
  }, kind)
}

async function settled(page, id, status = 'waiting_for_input') {
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, id), {
      timeout: 20_000,
    })
    .toBe(status)
  return page.evaluate((id) => window.harness.getRun(id), id)
}

function userBodies(request) {
  return request.messages
    .filter((message) => message.role === 'user')
    .map((message) =>
      typeof message.content === 'string' ? message.content : JSON.stringify(message.content),
    )
    .join('\n')
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
  const draftFiles = () => page.locator('.composer .file-references li')
  await expect(page.getByRole('button', { name: '添加文件', exact: true })).toBeVisible()
  await diskFileEvent(page, [firstPath])
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles()).toContainText('计划 sample.ts')
  await expect(page.locator('.composer')).not.toHaveClass(/composer-dragging/)
  await diskFileEvent(page, [firstPath])
  await expect(draftFiles()).toHaveCount(1)
  await page.getByRole('button', { name: '移除文件 计划 sample.ts', exact: true }).click()
  await expect(draftFiles()).toHaveCount(0)
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()
  await diskFileEvent(page, [firstPath])
  await page.getByRole('link', { name: /^工作区设置/ }).click()
  await page.getByRole('link', { name: '返回任务', exact: true }).click()
  await expect(draftFiles()).toHaveCount(1)

  await diskFileEvent(page, [imagePath])
  await expect(page.getByRole('alert')).toContainText('暂不支持')
  await expect(draftFiles()).toHaveCount(1)
  await diskFileEvent(page, [largePath])
  await expect(page.getByRole('alert')).toContainText('256')
  await expect(draftFiles()).toHaveCount(1)
  await diskFileEvent(page, [firstPath])
  await expect(page.getByRole('alert')).toHaveCount(0)
  assert(
    !(await page.evaluate(() => localStorage.getItem('open-harness-workspace'))).includes(
      'CREATE_FILE_VERSION_1',
    ),
    'unsent file contents must not enter localStorage',
  )
  await page.screenshot({ path: join(artifacts, 'desktop-file-references-draft.png') })

  // An admission error keeps both the typed message and imported snapshot.
  const description = page.getByRole('textbox', { name: '任务描述', exact: true })
  const invalidDraft = 'x'.repeat(4001)
  await description.fill(invalidDraft)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('4,000')
  await expect(description).toHaveValue(invalidDraft)
  await expect(draftFiles()).toHaveCount(1)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 0)
  await description.fill('')
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeEnabled()
  await writeFile(firstPath, 'CREATE_FILE_VERSION_2 should not replace the imported snapshot.\n')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const id = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  let detail = await settled(page, id)
  assert.equal(detail.title, '计划 sample.ts')
  assert.equal(detail.prompt, '')
  assert.equal(detail.workspace_path, undefined)
  assert.equal(detail.messages[0].files[0].size, Buffer.byteLength(firstText))
  assert(!JSON.stringify(detail).includes('CREATE_FILE_VERSION_1'))
  assert(userBodies(calls[0]).includes('CREATE_FILE_VERSION_1'))
  assert(!userBodies(calls[0]).includes('CREATE_FILE_VERSION_2'))
  await expect(draftFiles()).toHaveCount(0)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(1)

  await clipboardBackup()
  await application.evaluate(async ({ clipboard, ClipboardItem }, uri) => {
    await clipboard.write([new ClipboardItem({ 'text/uri-list': uri })])
  }, pathToFileURL(secondPath).href)
  await page.getByRole('textbox', { name: '任务描述', exact: true }).focus()
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles()).toContainText('clipboard-notes.md')
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(draftFiles()).toHaveCount(1)

  await application.evaluate(async ({ clipboard }) =>
    clipboard.writeText('普通粘贴文本 NORMAL_PASTE'),
  )
  const followup = page.getByRole('textbox', { name: '任务描述', exact: true })
  await followup.fill('prefix ')
  await followup.press('End')
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(followup).toHaveValue('prefix 普通粘贴文本 NORMAL_PASTE')
  assert.equal(
    await followup.evaluate((textarea) => {
      const clipboardData = new DataTransfer()
      clipboardData.setData('text/plain', 'https://example.com/reference')
      clipboardData.setData('text/uri-list', 'https://example.com/reference')
      const event = new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true })
      textarea.dispatchEvent(event)
      return event.defaultPrevented
    }),
    false,
    'ordinary web links must retain native paste even with a URI-list representation',
  )
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(2)
  detail = await settled(page, id)
  assert.equal(
    detail.messages.filter((message) => message.role === 'user')[1].files[0].name,
    'clipboard-notes.md',
  )
  assert(userBodies(calls[1]).includes('PASTE_FILE_SNAPSHOT'))

  holdNext = true
  await followup.fill('Pause this request so the next message exercises resume.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(3)
  await page.getByRole('button', { name: '停止运行', exact: true }).click()
  await settled(page, id, 'cancelled')
  await diskFileEvent(page, [thirdPath], 'paste')
  await expect(draftFiles()).toContainText('resume.txt')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(4)
  detail = await settled(page, id)
  assert(detail.events.some((event) => event.type === 'run_resumed'))
  assert(userBodies(calls.at(-1)).includes('RESUME_FILE_SNAPSHOT'))
  await expect(page.locator('.message-user .file-references li')).toHaveCount(3)
  await page.screenshot({ path: join(artifacts, 'desktop-file-references-conversation.png') })
  await closeApplication()

  page = await launch()
  const config = await page.evaluate(() => window.harness.config())
  if (!config.models.find((model) => model.id === configured.model.id)?.available)
    await page.evaluate((input) => window.harness.configureModel(input), configuration)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  detail = await settled(page, id)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(3)
  assert.equal(
    detail.messages.find((message) => message.role === 'user').files[0].size,
    Buffer.byteLength(firstText),
  )
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Check the saved snapshots after restart.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(5)
  await settled(page, id)
  const afterRestart = userBodies(calls.at(-1))
  for (const token of ['CREATE_FILE_VERSION_1', 'PASTE_FILE_SNAPSHOT', 'RESUME_FILE_SNAPSHOT'])
    assert(afterRestart.includes(token), `saved snapshot missing: ${token}`)
  assert(!afterRestart.includes('CREATE_FILE_VERSION_2'))
  const events = await readFile(
    join(dataDirectory, 'runtime', 'events', `run-${Buffer.from(id).toString('hex')}.jsonl`),
    'utf8',
  )
  assert(events.includes('CREATE_FILE_VERSION_1'))
  assert(!events.includes('CREATE_FILE_VERSION_2'))
  assert.deepEqual(errors, [], 'renderer must not report uncaught errors')
  assert.deepEqual(fixtureErrors, [])
  console.log(
    JSON.stringify({
      result: 'passed',
      run_id: id,
      provider_requests: calls.length,
      snapshots: 3,
      data_directory: dataDirectory,
    }),
  )
} finally {
  await closeApplication()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
