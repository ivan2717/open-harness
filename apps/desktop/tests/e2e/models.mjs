import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, readdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-model-e2e-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const calls = []
const metadataCalls = []
let failMetadata = false
let holdSummary = false
const summary = JSON.stringify({
  objective: 'Continue testing the workspace',
  constraints: ['Keep PROJECT-CEDAR'],
  completed_work: ['Two replies'],
  current_state: 'Ready',
  decisions: [],
  important_artifacts: [],
  failed_attempts: [],
  unresolved_questions: [],
  current_plan: [],
  next_action: 'Answer the next user',
})
const server = createServer(async (request, response) => {
  if (request.method === 'GET') {
    metadataCalls.push(request.url)
    response.writeHead(failMetadata ? 503 : 200, { 'Content-Type': 'application/json' })
    response.end(
      JSON.stringify(
        failMetadata
          ? { error: 'Metadata temporarily unavailable' }
          : request.url === '/v1/models/fixture-messages-metadata'
            ? { id: 'fixture-messages-metadata', max_input_tokens: 98304, max_tokens: 3072 }
            : {
                data: [
                  { id: 'fixture-metadata', context_window: 262144, max_output_tokens: 4096 },
                  { id: 'unrelated-model', context_window: 2097152, max_output_tokens: 65536 },
                ],
              },
      ),
    )
    return
  }
  assert.equal(request.method, 'POST')
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  const body = JSON.parse(Buffer.concat(chunks).toString())
  const isSummary = JSON.stringify(body).includes('Return only one JSON object')
  calls.push({ path: request.url, body, summary: isSummary })
  if (isSummary && holdSummary) return // cancellation must drop this HTTP request
  const text = isSummary
    ? summary
    : `PROJECT-CEDAR remains active. ${'This is a diagnostic observation. '.repeat(80)}`
  const usage = { input_tokens: 1200, output_tokens: 720 }
  response.writeHead(200, { 'Content-Type': 'text/event-stream' })
  const send = (data) => response.write(`data: ${JSON.stringify(data)}\n\n`)
  if (request.url.endsWith('/chat/completions')) {
    send({
      id: 'c',
      choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: 'stop' }],
    })
    send({
      choices: [],
      usage: { prompt_tokens: usage.input_tokens, completion_tokens: usage.output_tokens },
    })
    response.write('data: [DONE]\n\n')
  } else if (request.url.endsWith('/responses')) {
    send({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text })
    send({
      type: 'response.completed',
      response: {
        id: 'resp',
        status: 'completed',
        output: [
          {
            id: 'message',
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text }],
          },
        ],
        usage,
      },
    })
  } else if (request.url.endsWith('/messages')) {
    send({
      type: 'message_start',
      message: {
        id: 'msg',
        role: 'assistant',
        content: [],
        usage: { input_tokens: 1200, output_tokens: 0 },
      },
    })
    send({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } })
    send({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } })
    send({ type: 'content_block_stop', index: 0 })
    send({
      type: 'message_delta',
      delta: { stop_reason: 'end_turn' },
      usage: { output_tokens: 720 },
    })
    send({ type: 'message_stop' })
  } else assert.fail(`Unexpected provider route ${request.url}`)
  response.end()
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const base_url = `http://127.0.0.1:${server.address().port}/v1`
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application
const errors = []
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
async function settled(page, id) {
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).status, {
      timeout: 20_000,
    })
    .toBe('waiting_for_input')
  return page.evaluate((id) => window.harness.getRun(id), id)
}
const saved = []
const capacityRuns = []
const basicInput = {
  protocol: 'chat_completions',
  base_url,
  api_key: 'fixture-secret-do-not-persist-plain',
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
async function checkCapacity(page, model, input, windowTokens, output) {
  const before = calls.length
  const run = await page.evaluate(
    (model_id) => window.harness.createRun({ model_id, prompt: 'Check automatic model capacity.' }),
    model.id,
  )
  const complete = await settled(page, run.id)
  assert.equal(complete.context.window_tokens, windowTokens)
  assert.equal(complete.context.reserved_output_tokens, output)
  const request = calls.slice(before).find((call) => !call.summary).body
  assert.equal(
    request.max_tokens ?? request.max_completion_tokens ?? request.max_output_tokens,
    output,
  )
  capacityRuns.push({
    id: run.id,
    model_id: model.id,
    input: { ...input, context_window: windowTokens, max_output_tokens: output },
  })
}
async function saveUiModel(page, expectedWindow) {
  await page.getByRole('button', { name: '保存并使用', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('模型已接入')
  const selected = () =>
    page.evaluate(async () => {
      const selected = JSON.parse(localStorage.getItem('open-harness-workspace')).state
        .selectedModel
      return (await window.harness.config()).models.find((model) => model.id === selected)
    })
  await expect.poll(async () => (await selected())?.context_window).toBe(expectedWindow)
  return selected()
}
try {
  let page = await launch()
  await expect(page.getByRole('link', { name: '添加模型', exact: true })).toBeVisible()
  await expect(page.getByLabel('选择模型')).toHaveCount(0)
  await page.getByRole('link', { name: '添加模型', exact: true }).click()
  await expect(page.getByRole('heading', { name: '默认模型', exact: true })).toHaveCount(0)
  const context = page.getByLabel('上下文窗口（tokens）')
  const output = page.getByLabel('单次输出上限（tokens）')
  await expect(context).toBeHidden()
  await expect(output).toBeHidden()
  await page.getByLabel('API 基础地址').fill('https://api.openai.com/v1')
  await page.getByLabel('模型名称', { exact: true }).fill('gpt-6-luna')
  await page.locator('.model-advanced > summary').click()
  await expect(context).toHaveValue('')
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)1050000(?:）|$)/)
  assert.equal(metadataCalls.length, 0, 'typing a known model must not send a provider request')
  await context.fill('32000')
  await output.fill('2048')
  await page.getByLabel('模型名称', { exact: true }).fill('fixture-chat')
  await expect(context).toHaveValue('')
  await expect(output).toHaveValue('')
  await context.fill('32000')
  await output.fill('2048')
  await page.getByLabel('API 基础地址').fill(base_url)
  await expect(context).toHaveValue('')
  await expect(output).toHaveValue('')
  await context.fill('32000')
  await output.fill('2048')
  await page.getByLabel('接口协议').selectOption('responses')
  await expect(context).toHaveValue('')
  await expect(output).toHaveValue('')
  await page.getByLabel('接口协议').selectOption('chat_completions')
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)8192(?:）|$)/)
  await expect(output).toHaveAttribute('placeholder', /(?:^|（)2048(?:）|$)/)
  await page.locator('.model-advanced > summary').click()
  await page.getByLabel('API key', { exact: true }).fill('fixture-secret-do-not-persist-plain')
  const initialModel = await saveUiModel(page, 8192)
  await expect(page.getByLabel('API key', { exact: true })).toHaveValue('')
  assert.equal(calls.length, 0, 'saving a model must not make a generation request')
  assert(metadataCalls.includes('/v1/models'))
  await page.locator('.model-form').scrollIntoViewIfNeeded()
  await expect(page.locator('.model-advanced > summary')).toBeInViewport()
  await page.screenshot({ path: join(artifacts, 'desktop-model-settings.png') })
  const config = await page.evaluate(() => window.harness.config())
  assert(!JSON.stringify(config).includes('fixture-secret'))
  await checkCapacity(page, initialModel, { ...basicInput, model: 'fixture-chat' }, 8192, 2048)
  await page.getByRole('link', { name: '返回任务', exact: true }).click()
  await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
    initialModel.name,
  )
  await page.getByRole('button', { name: '选择模型', exact: true }).click()
  await expect(
    page.getByRole('menuitemradio', { name: initialModel.name, exact: true }),
  ).toHaveAttribute('aria-checked', 'true')
  await expect(page.getByRole('menuitemradio', { name: /Local demo/ })).toHaveCount(0)
  await page.keyboard.press('Escape')
  await page.evaluate(() => {
    const stored = JSON.parse(localStorage.getItem('open-harness-workspace'))
    stored.state.selectedModel = 'local-demo'
    localStorage.setItem('open-harness-workspace', JSON.stringify(stored))
  })
  await page.reload()
  await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
    initialModel.name,
  )
  await page.getByRole('button', { name: '选择模型', exact: true }).click()
  await expect(
    page.getByRole('menuitemradio', { name: initialModel.name, exact: true }),
  ).toHaveAttribute('aria-checked', 'true')
  await expect(page.getByRole('menuitemradio', { name: /Local demo/ })).toHaveCount(0)
  await page.keyboard.press('Escape')
  await page.getByRole('link', { name: /^工作区设置/ }).click()
  await page.getByLabel('API 基础地址').fill(base_url)
  await page.getByLabel('模型名称', { exact: true }).fill('fixture-metadata')
  await page.getByLabel('API key', { exact: true }).fill(basicInput.api_key)
  await page.locator('.model-advanced > summary').click()
  await context.fill('64000')
  await output.fill('3072')
  const beforeOverride = metadataCalls.length
  const overridden = await saveUiModel(page, 64000)
  assert.equal(metadataCalls.length, beforeOverride, 'two explicit limits skip metadata lookup')
  await page.screenshot({ path: join(artifacts, 'desktop-model-advanced.png') })
  await checkCapacity(page, overridden, { ...basicInput, model: 'fixture-metadata' }, 64000, 3072)
  await page.getByRole('button', { name: '恢复自动设置', exact: true }).click()
  await expect(context).toHaveValue('')
  await expect(output).toHaveValue('')
  await page.getByLabel('API key', { exact: true }).fill(basicInput.api_key)
  const automatic = await saveUiModel(page, 262144)
  assert.notEqual(automatic.id, overridden.id, 'capacity changes create a distinct replay identity')
  assert(metadataCalls.length > beforeOverride)
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)262144(?:）|$)/)
  await expect(output).toHaveAttribute('placeholder', /(?:^|（)4096(?:）|$)/)
  await output.fill('2048')
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)262144(?:）|$)/)
  await expect(page.locator('.model-advanced')).toContainText('已采用服务商提供的容量。')
  await page.getByRole('button', { name: '恢复自动设置', exact: true }).click()
  await expect(context).toHaveValue('')
  await expect(output).toHaveValue('')
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)262144(?:）|$)/)
  await expect(output).toHaveAttribute('placeholder', /(?:^|（)4096(?:）|$)/)
  await expect(page.locator('.model-advanced')).toContainText('已采用服务商提供的容量。')
  await page.screenshot({ path: join(artifacts, 'desktop-model-auto.png') })
  await page.getByLabel('模型名称', { exact: true }).fill('fixture-preview-reset')
  await expect(context).toHaveAttribute('placeholder', /(?:^|（)8192(?:）|$)/)
  await expect(output).toHaveAttribute('placeholder', /(?:^|（)2048(?:）|$)/)
  await checkCapacity(page, automatic, { ...basicInput, model: 'fixture-metadata' }, 262144, 4096)
  const metadata = await page.evaluate((input) => window.harness.configureModel(input), {
    ...basicInput,
    model: 'fixture-metadata',
  })
  assert.equal(metadata.model.id, automatic.id)
  assert.equal(metadata.limits.context_source, 'metadata')
  assert.equal(metadata.limits.output_source, 'metadata')
  assert.deepEqual(metadata.capacity, { context_window: 262144, max_output_tokens: 4096 })
  const missing = await page.evaluate((input) => window.harness.configureModel(input), {
    ...basicInput,
    model: 'fixture-chat',
  })
  assert.equal(missing.model.id, initialModel.id)
  assert.equal(missing.limits.context_source, 'fallback')
  assert.equal(missing.limits.output_source, 'fallback')
  let unavailable
  failMetadata = true
  try {
    unavailable = await page.evaluate((input) => window.harness.configureModel(input), {
      ...basicInput,
      model: 'fixture-unavailable',
    })
  } finally {
    failMetadata = false
  }
  assert.equal(unavailable.limits.context_source, 'fallback')
  assert.equal(unavailable.limits.output_source, 'fallback')
  await checkCapacity(
    page,
    unavailable.model,
    { ...basicInput, model: 'fixture-unavailable' },
    8192,
    2048,
  )
  const messagesInput = {
    ...basicInput,
    protocol: 'anthropic_messages',
    model: 'fixture-messages-metadata',
  }
  const messagesModel = await page.evaluate(
    (input) => window.harness.configureModel(input),
    messagesInput,
  )
  assert.equal(messagesModel.limits.context_source, 'metadata')
  assert.equal(messagesModel.limits.output_source, 'metadata')
  assert.deepEqual(messagesModel.capacity, { context_window: 98304, max_output_tokens: 3072 })
  assert(metadataCalls.includes('/v1/models/fixture-messages-metadata'))
  await checkCapacity(page, messagesModel.model, messagesInput, 98304, 3072)
  await page.getByRole('link', { name: '返回任务', exact: true }).click()
  for (const protocol of ['chat_completions', 'responses', 'anthropic_messages']) {
    const input = {
      protocol,
      base_url,
      api_key: 'fixture-secret-do-not-persist-plain',
      model: `fixture-${protocol}`,
      context_window: 32000,
      max_output_tokens: 2048,
      reasoning_mode: 'off',
      reasoning_effort: 'medium',
    }
    const configured = await page.evaluate((input) => window.harness.configureModel(input), input)
    // Direct bridge calls bypass the settings mutation's query invalidation.
    await page.reload()
    if (protocol === 'chat_completions') {
      const draft = 'Keep this draft when switching models.'
      await page.getByRole('textbox', { name: '任务描述', exact: true }).fill(draft)
      await page.getByRole('button', { name: '选择模型', exact: true }).click()
      await page.getByRole('menuitemradio', { name: configured.model.name, exact: true }).click()
      await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
        configured.model.name,
      )
      await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(draft)
    }
    const run = await page.evaluate(
      (model_id) =>
        window.harness.createRun({
          prompt: `Remember PROJECT-CEDAR. ${'Historical observation. '.repeat(80)}`,
          model_id,
        }),
      configured.model.id,
    )
    await settled(page, run.id)
    await page.evaluate(
      (id) => window.harness.submitInput(id, 'Keep the project code and continue.'),
      run.id,
    )
    await settled(page, run.id)
    if (protocol === 'chat_completions') {
      await page.evaluate((id) => {
        window.location.hash = `/runs/${id}`
      }, run.id)
      holdSummary = true
      await expect(page.getByRole('button', { name: '压缩上下文', exact: true })).toHaveCount(0)
      await page.getByRole('button', { name: '打开运行详情', exact: true }).click()
      await page.getByRole('tab', { name: '上下文', exact: true }).click()
      await page.getByRole('button', { name: '压缩上下文', exact: true }).click()
      await expect.poll(() => calls.filter((call) => call.summary).length).toBeGreaterThan(0)
      await page.getByRole('button', { name: '取消压缩', exact: true }).click()
      await expect(page.getByRole('button', { name: '压缩上下文', exact: true })).toBeEnabled()
      await page.screenshot({ path: join(artifacts, 'desktop-context-controls.png') })
      await page.getByRole('tab', { name: /时间线/ }).click()
      await expect(page.getByRole('button', { name: '压缩上下文', exact: true })).toHaveCount(0)
      await page.getByRole('tab', { name: '上下文', exact: true }).click()
      assert.equal(
        (await page.evaluate((id) => window.harness.getRun(id), run.id)).status,
        'waiting_for_input',
      )
      holdSummary = false
    }
    await page.evaluate((id) => window.harness.compactRun(id), run.id)
    await expect
      .poll(
        async () => (await page.evaluate((id) => window.harness.getRun(id), run.id)).compacting,
        { timeout: 20_000 },
      )
      .toBe(false)
    const compacted = await page.evaluate((id) => window.harness.getRun(id), run.id)
    assert.equal(compacted.error, undefined, compacted.error)
    assert(compacted.context.compaction_count > 0, `${protocol} must save a summary`)
    assert(
      compacted.messages.some((m) => m.text.includes('Remember PROJECT-CEDAR')),
      'canonical user history survives',
    )
    saved.push({
      input,
      id: run.id,
      model_id: configured.model.id,
      persisted: configured.persisted,
    })
  }
  const metadataBeforeRestart = metadataCalls.length
  await application.close()
  application = undefined
  page = await launch()
  const restarted = await page.evaluate(() => window.harness.config())
  assert.equal(metadataCalls.length, metadataBeforeRestart, 'restart must reuse resolved limits')
  for (const item of capacityRuns) {
    const model = restarted.models.find((model) => model.id === item.model_id)
    assert(model, 'the exact configured identity survives restart')
    assert.equal(model.context_window, item.input.context_window)
    if (!model.available) {
      const restored = await page.evaluate(
        (input) => window.harness.configureModel(input),
        item.input,
      )
      assert.equal(restored.model.id, item.model_id)
    }
    await page.evaluate(
      (id) => window.harness.submitInput(id, 'Continue the capacity check.'),
      item.id,
    )
    const run = await settled(page, item.id)
    assert.equal(run.context.window_tokens, item.input.context_window)
    assert.equal(run.context.reserved_output_tokens, item.input.max_output_tokens)
  }
  assert.equal(
    metadataCalls.length,
    metadataBeforeRestart,
    'restoring explicit limits never probes',
  )
  for (const item of saved) {
    if (!item.persisted) {
      const state = await page.evaluate(() => window.harness.config())
      assert.equal(state.models.find((m) => m.id === item.model_id).available, false)
      await assert.rejects(
        page.evaluate((id) => window.harness.submitInput(id, 'continue'), item.id),
      )
      const restored = await page.evaluate(
        (input) => window.harness.configureModel(input),
        item.input,
      )
      assert.equal(restored.model.id, item.model_id)
    }
    await page.evaluate(
      (id) => window.harness.submitInput(id, 'What is the project code?'),
      item.id,
    )
    const run = await settled(page, item.id)
    assert(run.messages.at(-1).text.includes('PROJECT-CEDAR'))
    // First omission exposes history-read tool schemas, changing the fixed
    // header. The following response must establish a new durable baseline.
    await page.evaluate(
      (id) => window.harness.submitInput(id, 'Confirm the project code.'),
      item.id,
    )
    const calibrated = await settled(page, item.id)
    assert.equal(calibrated.context.accounting.source, 'provider_calibrated')
  }
  for (const file of await readdir(join(directory, 'runtime', 'runs'))) {
    assert(
      !(await readFile(join(directory, 'runtime', 'runs', file), 'utf8')).includes(
        'fixture-secret',
      ),
    )
  }
  assert(
    calls.every((call) =>
      ['/v1/chat/completions', '/v1/responses', '/v1/messages'].includes(call.path),
    ),
    'no token-count requests',
  )
  assert(
    metadataCalls.every((path) =>
      ['/v1/models', '/v1/models/fixture-messages-metadata'].includes(path),
    ),
    'metadata uses only model information routes',
  )
  assert.equal(errors.length, 0, errors.join('\n'))
  console.log(
    JSON.stringify({
      result: 'passed',
      protocols: saved.length,
      capacity_cases: capacityRuns.length,
      metadata_requests: metadataCalls.length,
      provider_requests: calls.length,
      summary_requests: calls.filter((call) => call.summary).length,
      data_directory: directory,
    }),
  )
} finally {
  await application?.close()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
