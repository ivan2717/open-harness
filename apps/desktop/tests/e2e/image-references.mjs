import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-image-e2e-'))
const dataDirectory = join(directory, 'app-data')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const diskPath = join(directory, 'selected-screenshot.png')
const memoryName = 'clipboard-memory.png'
const followupName = 'followup-screenshot.png'
const calls = []
const errors = []
const fixtureErrors = []
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
    calls.push(JSON.parse(Buffer.concat(chunks).toString()))
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.write(
      `data: ${JSON.stringify({
        id: `images-${calls.length}`,
        choices: [
          {
            index: 0,
            delta: { role: 'assistant', content: 'Image snapshots received.' },
            finish_reason: 'stop',
          },
        ],
      })}\n\n`,
    )
    response.write(
      `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 1000, completion_tokens: 10 } })}\n\n`,
    )
    response.end('data: [DONE]\n\n')
  } catch (error) {
    fixtureErrors.push(error.message)
    response.writeHead(500)
    response.end('Synthetic fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  model: 'image-reference-fixture',
  api_key: 'synthetic-image-fixture-key',
  context_window: 64000,
  max_output_tokens: 2048,
  supports_images: true,
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
    // Materialize lazy clipboard payloads before writing. The original content
    // stays inside this main process and is neither logged nor serialized.
    const backup = []
    for (const item of await clipboard.read()) {
      const values = {}
      for (const type of item.types) values[type] = await item.getType(type)
      if (item.types.length) backup.push(new ClipboardItem(values))
    }
    globalThis.__imageReferenceClipboardBackup = backup
  })
  clipboardBackedUp = true
}

async function restoreClipboard() {
  if (!application || !clipboardBackedUp) return
  await application.evaluate(async ({ clipboard }) => {
    await clipboard.write(globalThis.__imageReferenceClipboardBackup)
    delete globalThis.__imageReferenceClipboardBackup
  })
  clipboardBackedUp = false
}

async function closeApplication() {
  await restoreClipboard()
  await application?.close()
  application = undefined
}

async function memoryPaste(page, name, base64, text = '') {
  const textarea = page.getByRole('textbox', { name: '任务描述', exact: true })
  await expect(textarea).toBeEditable()
  const result = await textarea.evaluate(
    (textarea, { name, base64, text }) => {
      const bytes = Uint8Array.from(atob(base64), (character) => character.charCodeAt(0))
      const file = new File([bytes], name, { type: 'image/png' })
      const data = new DataTransfer()
      data.items.add(file)
      if (text) data.setData('text/plain', text)
      const event = new ClipboardEvent('paste', {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      })
      textarea.dispatchEvent(event)
      return { prevented: event.defaultPrevented, hasPath: 'path' in file }
    },
    { name, base64, text },
  )
  assert.equal(result.hasPath, false, 'clipboard image fixture must have no original disk path')
  assert.equal(result.prevented, true, 'image paste must enter the attachment flow')
}

async function dropDiskImage(page) {
  await page.evaluate(() => {
    const input = document.createElement('input')
    input.type = 'file'
    input.hidden = true
    input.id = 'image-reference-test-input'
    document.body.append(input)
  })
  const input = page.locator('#image-reference-test-input')
  await input.setInputFiles(diskPath)
  await input.evaluate((input) => {
    const data = new DataTransfer()
    data.items.add(input.files[0])
    const composer = document.querySelector('.composer')
    for (const type of ['dragenter', 'drop'])
      composer.dispatchEvent(
        new DragEvent(type, { dataTransfer: data, bubbles: true, cancelable: true }),
      )
    input.remove()
  })
}

async function chooseModel(page, model) {
  const picker = page.getByRole('button', { name: '选择模型', exact: true })
  await picker.click()
  await page
    .getByRole('group', { name: '模型', exact: true })
    .getByRole('menuitemradio', { name: model.name, exact: true })
    .click()
  await expect(picker).toContainText(model.name)
}

async function settled(page, id) {
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, id), {
      timeout: 20_000,
    })
    .toBe('waiting_for_input')
  return page.evaluate((id) => window.harness.getRun(id), id)
}

async function previewImage(page, name, sent = false) {
  await page
    .getByRole('button', { name: `${sent ? '预览文件' : '预览图片'} ${name}`, exact: true })
    .click()
  const dialog = page.getByRole('dialog', { name, exact: true })
  await expect(dialog).toBeVisible()
  await expect
    .poll(() => dialog.locator('img').evaluate((image) => image.complete && image.naturalWidth > 0))
    .toBe(true)
  await dialog.getByRole('button', { name: '关闭预览', exact: true }).click()
  await expect(dialog).toHaveCount(0)
}

function imageUrls(body) {
  const userBlocks = body.messages
    .filter((message) => message.role === 'user')
    .flatMap((message) => (Array.isArray(message.content) ? message.content : []))
  assert(
    !userBlocks.some((block) => block.type === 'text' && block.text.includes('base64,')),
    'image bytes must never be injected as prompt text',
  )
  return userBlocks
    .filter((block) => block.type === 'image_url')
    .map((block) => {
      assert.equal(block.image_url.detail, 'high')
      assert.match(block.image_url.url, /^data:image\/png;base64,/)
      return block.image_url.url
    })
}

async function assertNoPersistentDraftPixels(page, snapshots) {
  const stored = await page.evaluate(() =>
    Object.keys(localStorage)
      .map((key) => localStorage.getItem(key))
      .join('\n'),
  )
  assert(!stored.includes('data:image/'), 'image previews must not be saved to localStorage')
  for (const snapshot of snapshots)
    assert(
      !stored.includes(snapshot.split(',')[1]),
      'raw image bytes must not be saved to localStorage',
    )
}

try {
  let page = await launch()
  // Generate every image from synthetic pixel buffers, never from the screen or user files.
  const images = await application.evaluate(({ nativeImage }) =>
    Array.from({ length: 4 }, (_, variant) => {
      const width = 16
      const height = 12
      const pixels = Buffer.alloc(width * height * 4)
      for (let offset = 0; offset < pixels.length; offset += 4) {
        pixels[offset] = (offset + variant * 51) % 256
        pixels[offset + 1] = (variant * 63 + 70) % 256
        pixels[offset + 2] = (offset / 4 + variant * 27) % 256
        pixels[offset + 3] = 255
      }
      return nativeImage.createFromBitmap(pixels, { width, height }).toPNG().toString('base64')
    }),
  )
  await writeFile(diskPath, Buffer.from(images[1], 'base64'))
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  const vision = await page.evaluate((input) => window.harness.configureModel(input), configuration)
  const textOnly = await page.evaluate((input) => window.harness.configureModel(input), {
    ...configuration,
    model: 'text-only-fixture',
    supports_images: false,
  })
  await page.evaluate(
    (selectedModel) =>
      localStorage.setItem(
        'open-harness-workspace',
        JSON.stringify({
          state: { drafts: {}, selectedModel, inspectorOpen: false, inspectorTab: 'context' },
          version: 1,
        }),
      ),
    vision.model.id,
  )
  await page.reload()
  const draftFiles = () => page.locator('.composer .file-references li')
  const submit = () => page.getByRole('button', { name: '开始运行', exact: true })

  await memoryPaste(page, memoryName, images[0])
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles().locator('img')).toHaveCount(1)
  await previewImage(page, memoryName)
  await chooseModel(page, textOnly.model)
  await expect(page.getByRole('alert')).toContainText('当前模型未启用图片输入')
  await expect(submit()).toBeDisabled()
  await expect(draftFiles()).toHaveCount(1)
  assert.equal(calls.length, 0)
  await chooseModel(page, vision.model)
  await expect(submit()).toBeEnabled()
  await page.getByRole('button', { name: `移除文件 ${memoryName}`, exact: true }).click()
  await expect(draftFiles()).toHaveCount(0)
  await expect(submit()).toBeDisabled()
  const description = page.getByRole('textbox', { name: '任务描述', exact: true })
  await description.fill('prefix old suffix')
  await description.evaluate((textarea) => textarea.setSelectionRange(7, 10))
  await memoryPaste(page, memoryName, images[0], 'PASTED_WITH_IMAGE')
  await expect(draftFiles()).toHaveCount(1)
  await expect(description).toHaveValue('prefix PASTED_WITH_IMAGE suffix')
  await description.fill('')

  await application.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] })
  }, diskPath)
  await page.getByRole('button', { name: '添加内容', exact: true }).click()
  await page.getByRole('menuitem', { name: '添加文件', exact: true }).click()
  await expect(draftFiles()).toHaveCount(2)
  await previewImage(page, 'selected-screenshot.png')
  await page.getByRole('button', { name: '移除文件 selected-screenshot.png', exact: true }).click()
  await expect(draftFiles()).toHaveCount(1)
  await dropDiskImage(page)
  await expect(draftFiles()).toHaveCount(2)

  await clipboardBackup()
  try {
    await application.evaluate(async ({ clipboard, ClipboardItem }, base64) => {
      await clipboard.write([
        new ClipboardItem({
          'image/png': new Blob([Buffer.from(base64, 'base64')], { type: 'image/png' }),
        }),
      ])
    }, images[2])
    await page.getByRole('textbox', { name: '任务描述', exact: true }).focus()
    await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
    await expect(draftFiles()).toHaveCount(3)
  } finally {
    await restoreClipboard()
  }
  await expect(draftFiles().locator('img')).toHaveCount(3)
  const snapshots = await draftFiles()
    .locator('img')
    .evaluateAll((images) => images.map((image) => image.src))
  await assertNoPersistentDraftPixels(page, snapshots)
  await page.screenshot({ path: join(artifacts, 'desktop-image-references-draft.png') })
  // Changing a selected file must not change the already imported image snapshot.
  await writeFile(diskPath, Buffer.from(images[3], 'base64'))
  await submit().click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const id = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  let detail = await settled(page, id)
  assert.equal(calls.length, 1)
  assert.deepEqual(imageUrls(calls[0]), snapshots)
  assert.equal(detail.prompt, '', 'image-only messages must be accepted')
  assert.equal(detail.messages[0].files.length, 3)
  assert(detail.messages[0].files.every((file) => file.mime_type === 'image/png' && file.size > 0))
  assert(
    !JSON.stringify(detail).includes('data_base64'),
    'run detail must contain attachment metadata only',
  )
  const runs = await page.evaluate(() => window.harness.listRuns())
  for (const snapshot of snapshots) {
    assert(!JSON.stringify(detail).includes(snapshot.split(',')[1]))
    assert(!JSON.stringify(runs).includes(snapshot.split(',')[1]))
  }
  await expect(draftFiles()).toHaveCount(0)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(3)
  await previewImage(page, memoryName, true)
  await previewImage(page, 'selected-screenshot.png', true)

  await memoryPaste(page, followupName, images[3])
  await expect(draftFiles()).toHaveCount(1)
  const followupSnapshot = await draftFiles().locator('img').getAttribute('src')
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Compare this new screenshot with the previous screenshots.')
  await submit().click()
  await expect.poll(() => calls.length).toBe(2)
  detail = await settled(page, id)
  const allSnapshots = [...snapshots, followupSnapshot]
  assert.deepEqual(imageUrls(calls[1]), allSnapshots)
  await assertNoPersistentDraftPixels(page, allSnapshots)
  await page.screenshot({ path: join(artifacts, 'desktop-image-references-conversation.png') })
  await closeApplication()

  page = await launch()
  const currentConfig = await page.evaluate(() => window.harness.config())
  if (!currentConfig.models.find((model) => model.id === vision.model.id)?.available)
    await page.evaluate((input) => window.harness.configureModel(input), configuration)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  await settled(page, id)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(4)
  await previewImage(page, memoryName, true)
  await previewImage(page, followupName, true)
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Check the saved screenshots after restarting.')
  await submit().click()
  await expect.poll(() => calls.length).toBe(3)
  await settled(page, id)
  assert.deepEqual(imageUrls(calls[2]), allSnapshots)
  await assertNoPersistentDraftPixels(page, allSnapshots)
  assert.deepEqual(errors, [], 'renderer must not report uncaught errors')
  assert.deepEqual(fixtureErrors, [])
  console.log(
    JSON.stringify({
      result: 'passed',
      run_id: id,
      provider_requests: calls.length,
      images: 4,
      data_directory: dataDirectory,
    }),
  )
} finally {
  try {
    await closeApplication()
  } finally {
    server.closeAllConnections()
    await new Promise((resolve) => server.close(resolve))
  }
}
