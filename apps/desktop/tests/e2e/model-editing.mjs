import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-model-edit-e2e-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const originalKey = 'synthetic-original-model-edit-key'
const replacementKey = 'synthetic-replacement-model-edit-key'
const errors = []
const fixtureErrors = []

async function fixtureProvider() {
  const calls = []
  const metadata = []
  const server = createServer(async (request, response) => {
    try {
      if (request.method === 'GET') {
        metadata.push({ path: request.url, authorization: request.headers.authorization })
        response.writeHead(200, { 'Content-Type': 'application/json' })
        response.end(JSON.stringify({ data: [] }))
        return
      }
      assert.equal(request.method, 'POST')
      assert.equal(request.url, '/v1/chat/completions')
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      calls.push({
        body: JSON.parse(Buffer.concat(chunks).toString()),
        authorization: request.headers.authorization,
      })
      response.writeHead(200, { 'Content-Type': 'text/event-stream' })
      response.write(
        `data: ${JSON.stringify({
          id: `model-edit-${calls.length}`,
          choices: [
            {
              index: 0,
              delta: { role: 'assistant', content: 'MODEL-EDIT-CONTEXT is remembered.' },
              finish_reason: 'stop',
            },
          ],
        })}\n\n`,
      )
      response.write(
        `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 1000, completion_tokens: 20 } })}\n\n`,
      )
      response.end('data: [DONE]\n\n')
    } catch (error) {
      fixtureErrors.push(error.message)
      response.writeHead(500)
      response.end('Synthetic fixture failed')
    }
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  return { server, calls, metadata, baseUrl: `http://127.0.0.1:${server.address().port}/v1` }
}

const first = await fixtureProvider()
const second = await fixtureProvider()
const configuration = {
  protocol: 'chat_completions',
  base_url: first.baseUrl,
  model: 'model-edit-original',
  api_key: originalKey,
  context_window: 64000,
  max_output_tokens: 2048,
  supports_images: false,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
  reasoning_capability: {
    source: 'manual',
    adapter: 'effort',
    options: [
      { id: 'low', effort: 'low' },
      { id: 'medium', effort: 'medium' },
      { id: 'high', effort: 'high' },
    ],
  },
  reasoning_default: null,
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application

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

async function closeApplication() {
  await application?.close()
  application = undefined
}

async function settled(page, id) {
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, id), {
      timeout: 20_000,
    })
    .toBe('waiting_for_input')
  return page.evaluate((id) => window.harness.getRun(id), id)
}

async function configurationFor(page, id) {
  const saved = await page.evaluate((id) => window.harness.getModelConfiguration(id), id)
  assert(
    !Object.hasOwn(saved, 'api_key'),
    'editable configuration must never expose saved credentials',
  )
  assert(!JSON.stringify(saved).includes(originalKey))
  assert(!JSON.stringify(saved).includes(replacementKey))
  return saved
}

function updateInput(saved, patch = {}) {
  const { id, has_api_key, ...input } = saved
  return { ...input, api_key: '', ...patch }
}

async function expectOnlyProfile(page, name, count) {
  const config = await page.evaluate(() => window.harness.config())
  assert.equal(
    config.models.length,
    count,
    'editing must replace the configured profile, not append a duplicate',
  )
  const matches = config.models.filter((model) => model.name === name)
  assert.equal(matches.length, 1)
  return matches[0]
}

async function visitRun(page, id) {
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toBeEditable()
}

async function openEditor(page, name) {
  await page.getByRole('link', { name: '模型', exact: true }).click()
  await page.getByRole('button', { name: `编辑 ${name} 的配置`, exact: true }).click()
  await expect(page.getByRole('heading', { name: '编辑模型', exact: true })).toBeVisible()
  await expect(page.getByLabel('API key', { exact: true })).toHaveValue('')
}

function assertVisionCall(call, key, model) {
  assert.equal(
    call.authorization,
    `Bearer ${key}`,
    'the request must use the expected saved credential',
  )
  assert.equal(call.body.model, model)
  assert(
    JSON.stringify(call.body).includes('MODEL-EDIT-CONTEXT'),
    'existing conversation history must survive editing',
  )
  const images = call.body.messages
    .filter((message) => message.role === 'user')
    .flatMap((message) => (Array.isArray(message.content) ? message.content : []))
    .filter((block) => block.type === 'image_url')
  assert.equal(images.length, 1, 'the submitted screenshot must survive each configuration update')
  assert.match(images[0].image_url.url, /^data:image\/png;base64,/)
}

try {
  let page = await launch()
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    configuration,
  )
  const modelCount = (await page.evaluate(() => window.harness.config())).models.length
  const created = await page.evaluate(
    (model_id) => window.harness.createRun({ model_id, prompt: 'Remember MODEL-EDIT-CONTEXT.' }),
    configured.model.id,
  )
  const originalRun = await settled(page, created.id)
  assert.equal(first.calls.length, 1)
  assert.equal(first.calls[0].authorization, `Bearer ${originalKey}`)
  const initialSaved = await configurationFor(page, configured.model.id)
  assert.equal(initialSaved.has_api_key, true)
  assert.equal(initialSaved.supports_images, false)
  // Setup uses IPC directly rather than the form mutation that invalidates the
  // renderer's configuration cache. Reload once before exercising actual UI.
  await page.reload()
  await visitRun(page, created.id)
  const syntheticPng = await application.evaluate(({ nativeImage }) => {
    const pixels = Buffer.alloc(16 * 12 * 4, 255)
    for (let offset = 0; offset < pixels.length; offset += 4) pixels[offset] = offset % 256
    return nativeImage
      .createFromBitmap(pixels, { width: 16, height: 12 })
      .toPNG()
      .toString('base64')
  })
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .evaluate((textarea, base64) => {
      const data = new DataTransfer()
      const bytes = Uint8Array.from(atob(base64), (character) => character.charCodeAt(0))
      data.items.add(new File([bytes], 'draft-for-model-edit.png', { type: 'image/png' }))
      textarea.dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }),
      )
    }, syntheticPng)
  await expect(page.locator('.composer .file-references li')).toHaveCount(1)
  await expect(page.getByRole('alert')).toContainText('当前模型未启用图片输入')
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()

  await openEditor(page, configured.model.name)
  await expect(page.getByLabel('API 基础地址')).toHaveValue(first.baseUrl)
  await expect(page.getByLabel('模型名称', { exact: true })).toHaveValue(configuration.model)
  await page.getByLabel('此模型支持图片输入', { exact: true }).check()
  await page.getByLabel('思考设置').selectOption('high')
  const advanced = page.locator('.model-advanced')
  if (!(await advanced.evaluate((details) => details.open)))
    await advanced.locator('summary').click()
  await page.getByLabel('上下文窗口（tokens）', { exact: true }).fill('128000')
  await page.getByLabel('单次输出上限（tokens）', { exact: true }).fill('4096')
  await page.getByRole('button', { name: '保存修改', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('模型配置已更新')
  await expect
    .poll(
      async () =>
        (await page.evaluate(() => window.harness.config())).models.find(
          (model) => model.name === configuration.model,
        )?.supports_images,
    )
    .toBe(true)
  let editedModel = await expectOnlyProfile(page, configuration.model, modelCount)
  let saved = await configurationFor(page, editedModel.id)
  assert.equal(saved.context_window, 128000)
  assert.equal(saved.max_output_tokens, 4096)
  assert.equal(saved.reasoning_default, 'high')
  assert.equal(saved.has_api_key, true)
  assert.equal(first.calls.length, 1, 'saving settings must not make a generation request')
  let run = await page.evaluate((id) => window.harness.getRun(id), created.id)
  assert.equal(
    run.model_id,
    editedModel.id,
    'the existing run must follow the edited model configuration',
  )
  assert.deepEqual(run.messages, originalRun.messages, 'editing must preserve conversation history')
  assert.equal(run.context.window_tokens, 128000)
  await visitRun(page, created.id)
  await expect(page.locator('.composer .file-references li')).toHaveCount(1)
  await expect(page.locator('.composer .file-references img')).toHaveCount(1)
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeEnabled()
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Use the screenshot with MODEL-EDIT-CONTEXT.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => first.calls.length).toBe(2)
  await settled(page, created.id)
  assertVisionCall(first.calls[1], originalKey, configuration.model)
  assert.equal(first.calls[1].body.reasoning_effort, 'high')
  assert.equal(first.calls[1].body.max_completion_tokens, 4096)

  // Cancel must discard form changes, including a typed replacement credential.
  await openEditor(page, editedModel.name)
  await page.getByLabel('模型名称', { exact: true }).fill('cancelled-edit-must-not-exist')
  await page.getByLabel('API key', { exact: true }).fill('synthetic-cancelled-key')
  await page.getByLabel('此模型支持图片输入', { exact: true }).uncheck()
  await page.getByRole('button', { name: '取消编辑', exact: true }).click()
  assert.deepEqual(await configurationFor(page, editedModel.id), saved)
  await expectOnlyProfile(page, configuration.model, modelCount)
  await openEditor(page, editedModel.name)
  await expect(page.getByLabel('此模型支持图片输入', { exact: true })).toBeChecked()
  await page.screenshot({ path: join(artifacts, 'desktop-model-editing.png') })
  await page.getByRole('button', { name: '取消编辑', exact: true }).click()

  const renamedName = 'model-edit-renamed'
  const renamed = await page.evaluate(
    ({ id, input }) => window.harness.updateModelConfiguration(id, input),
    {
      id: editedModel.id,
      input: updateInput(saved, { model: renamedName }),
    },
  )
  editedModel = await expectOnlyProfile(page, renamedName, modelCount)
  assert.equal(renamed.model.id, editedModel.id)
  saved = await configurationFor(page, editedModel.id)
  run = await page.evaluate((id) => window.harness.getRun(id), created.id)
  assert.equal(run.model_id, editedModel.id)
  await page.evaluate(
    (id) => window.harness.submitInput(id, 'Continue MODEL-EDIT-CONTEXT after renaming the model.'),
    created.id,
  )
  await settled(page, created.id)
  assert.equal(first.calls.length, 3)
  assertVisionCall(first.calls[2], originalKey, renamedName)

  // Changing a port changes the service origin. Never send the previous key to
  // the new origin, even during metadata discovery for a rejected edit.
  const refused = await page.evaluate(
    async ({ id, input }) => {
      try {
        await window.harness.updateModelConfiguration(id, input)
        return null
      } catch (error) {
        return String(error)
      }
    },
    { id: editedModel.id, input: updateInput(saved, { base_url: second.baseUrl }) },
  )
  assert.equal(typeof refused, 'string', 'changing origin with a blank key must be rejected')
  assert.equal(second.calls.length, 0)
  assert.equal(second.metadata.length, 0)
  assert.deepEqual(await configurationFor(page, editedModel.id), saved)
  const moved = await page.evaluate(
    ({ id, input }) => window.harness.updateModelConfiguration(id, input),
    {
      id: editedModel.id,
      input: updateInput(saved, { base_url: second.baseUrl, api_key: replacementKey }),
    },
  )
  editedModel = await expectOnlyProfile(page, renamedName, modelCount)
  assert.equal(moved.model.id, editedModel.id)
  saved = await configurationFor(page, editedModel.id)
  assert.equal(saved.base_url, second.baseUrl)
  assert(second.metadata.every((request) => request.authorization !== `Bearer ${originalKey}`))
  run = await page.evaluate((id) => window.harness.getRun(id), created.id)
  assert.equal(run.model_id, editedModel.id)
  await page.evaluate(
    (id) => window.harness.submitInput(id, 'Continue after changing the provider endpoint.'),
    created.id,
  )
  await settled(page, created.id)
  assert.equal(second.calls.length, 1)
  assertVisionCall(second.calls[0], replacementKey, renamedName)
  await closeApplication()

  page = await launch()
  const afterRestart = await configurationFor(page, editedModel.id)
  const { has_api_key: _beforeKey, ...beforeSettings } = saved
  const { has_api_key: afterKey, ...afterSettings } = afterRestart
  assert.deepEqual(
    afterSettings,
    beforeSettings,
    'the edited configuration must persist across restart',
  )
  if (moved.persisted)
    assert.equal(afterKey, true, 'an encrypted saved key must remain available after restart')
  if (!afterKey) {
    await page.evaluate(({ id, input }) => window.harness.updateModelConfiguration(id, input), {
      id: editedModel.id,
      input: updateInput(afterRestart, { api_key: replacementKey }),
    })
  }
  await expectOnlyProfile(page, renamedName, modelCount)
  run = await page.evaluate((id) => window.harness.getRun(id), created.id)
  assert.equal(run.model_id, editedModel.id, 'the migrated run binding must persist across restart')
  await visitRun(page, created.id)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(1)
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Confirm MODEL-EDIT-CONTEXT after restart.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => second.calls.length).toBe(2)
  await settled(page, created.id)
  assertVisionCall(second.calls[1], replacementKey, renamedName)
  assert.equal(second.calls[1].body.reasoning_effort, 'high')
  assert.equal(second.calls[1].body.max_completion_tokens, 4096)
  assert.deepEqual(errors, [])
  assert.deepEqual(fixtureErrors, [])
  console.log(
    JSON.stringify({
      result: 'passed',
      run_id: created.id,
      provider_requests: first.calls.length + second.calls.length,
      persisted_key: moved.persisted,
      data_directory: directory,
    }),
  )
} finally {
  try {
    await closeApplication()
  } finally {
    for (const provider of [first, second]) {
      provider.server.closeAllConnections()
      await new Promise((resolve) => provider.server.close(resolve))
    }
  }
}
