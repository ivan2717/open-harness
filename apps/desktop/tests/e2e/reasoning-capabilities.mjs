import assert from 'node:assert/strict'
import { expect } from '@playwright/test'

export const metadataReasoning = {
  adapter: 'deepseek',
  options: [
    { id: 'off', enabled: false },
    { id: 'low', effort: 'low' },
    { id: 'high', effort: 'high' },
    { id: 'max', effort: 'max' },
  ],
}

/** Validate saved capabilities through Electron IPC, the runtime and actual HTTP bodies. */
export async function exerciseReasoningCapabilities(page, { baseInput, calls, settled }) {
  const save = (input) => page.evaluate((input) => window.harness.configureModel(input), input)
  const base = { ...baseInput, context_window: 64000, max_output_tokens: 8192 }
  const beforeSave = calls.length
  const unknown = await save({ ...base, model: 'gpt-6-luna' })
  assert.deepEqual(unknown.model.reasoning.supported_efforts, ['low', 'medium', 'high'])
  assert.equal(unknown.model.reasoning.capability.source, 'preset')
  await assert.rejects(
    page.evaluate(
      (model_id) =>
        window.harness.createRun({ model_id, prompt: 'Invalid effort', reasoning_effort: 'ultra' }),
      unknown.model.id,
    ),
  )
  assert.equal(calls.length, beforeSave, 'discovery and validation never generate a model reply')

  const automaticInput = { ...base, model: 'fixture-reasoning-metadata' }
  const automatic = await save(automaticInput)
  assert.equal(automatic.model.reasoning.capability.source, 'metadata')
  assert.deepEqual(automatic.model.reasoning.supported_efforts, ['off', 'low', 'high', 'max'])
  assert.equal(automatic.model.reasoning.default_effort, null)
  assert.equal(
    calls.length,
    beforeSave,
    'explicit capacities do not prevent safe metadata discovery',
  )

  async function create(model) {
    const offset = calls.length
    const run = await page.evaluate(
      (model_id) => window.harness.createRun({ model_id, prompt: 'Remember CAPABILITY-CEDAR.' }),
      model.id,
    )
    await settled(page, run.id)
    return { run, body: calls.slice(offset).find((call) => !call.summary).body }
  }
  async function reply(id, model, effort) {
    await page.evaluate(
      ({ id, model_id, reasoning_effort }) =>
        window.harness.updateRunModel(id, { model_id, reasoning_effort }),
      { id, model_id: model.id, reasoning_effort: effort },
    )
    const offset = calls.length
    await page.evaluate((id) => window.harness.submitInput(id, 'Continue CAPABILITY-CEDAR.'), id)
    await settled(page, id)
    return calls.slice(offset).find((call) => !call.summary).body
  }
  const presetRun = await create(unknown.model)
  assert.equal(presetRun.body.reasoning_effort, undefined)
  assert.equal(presetRun.body.thinking, undefined)
  const presetBody = await reply(presetRun.run.id, unknown.model, 'high')
  assert.equal(presetBody.reasoning_effort, 'high')
  assert.equal(presetBody.thinking, undefined, 'a relay does not inherit a vendor dialect by name')
  const started = await create(automatic.model)
  assert.equal(started.body.thinking, undefined)
  assert.equal(started.body.reasoning_effort, undefined)
  let body = await reply(started.run.id, automatic.model, 'max')
  assert.deepEqual(body.thinking, { type: 'enabled' })
  assert.equal(body.reasoning_effort, 'max')
  body = await reply(started.run.id, automatic.model, 'off')
  assert.deepEqual(body.thinking, { type: 'disabled' })
  assert.equal(body.reasoning_effort, undefined)
  body = await reply(started.run.id, automatic.model, null)
  assert.equal(body.thinking, undefined, 'service default is different from explicit disabled')
  assert.equal(body.reasoning_effort, undefined)
  await assert.rejects(
    page.evaluate(
      ({ id, model_id }) =>
        window.harness.updateRunModel(id, { model_id, reasoning_effort: 'medium' }),
      { id: started.run.id, model_id: automatic.model.id },
    ),
  )

  const mappedInput = {
    ...base,
    protocol: 'responses',
    model: 'fixture-mapped-effort',
    reasoning_default: 'careful',
    reasoning_capability: {
      source: 'manual',
      adapter: 'effort',
      options: [
        { id: 'none', effort: 'none' },
        { id: 'careful', label: '仔细思考', effort: 'max' },
      ],
    },
  }
  const mapped = await save(mappedInput)
  const mappedRun = await create(mapped.model)
  assert.deepEqual(mappedRun.body.reasoning, { effort: 'max' })
  assert.equal(mappedRun.body.reasoning_effort, undefined)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, mappedRun.run.id)
  // Fixture setup calls the bridge directly rather than the settings mutation,
  // so refresh the renderer's cached model list before asserting its menu.
  await page.reload()
  const picker = page.getByRole('button', { name: '选择模型', exact: true })
  await expect(picker).toContainText('仔细思考')
  await picker.click()
  const group = page.getByRole('group', { name: '思考强度', exact: true })
  await expect(group.getByRole('menuitemradio', { name: '仔细思考', exact: true })).toBeVisible()
  await expect(group.getByRole('menuitemradio', { name: '高', exact: true })).toHaveCount(0)
  await page.keyboard.press('Escape')
  body = await reply(mappedRun.run.id, mapped.model, 'none')
  assert.deepEqual(body.reasoning, { effort: 'none' })

  const cases = [
    {
      protocol: 'anthropic_messages',
      adapter: 'messages_effort_only',
      options: [{ id: 'high', effort: 'high' }],
      selected: 'high',
      verify: (body) => {
        assert.deepEqual(body.output_config, { effort: 'high' })
        assert.equal(body.thinking, undefined)
      },
    },
    {
      protocol: 'chat_completions',
      adapter: 'thinking_toggle',
      options: [
        { id: 'on', enabled: true },
        { id: 'off', enabled: false },
      ],
      selected: 'on',
      verify: (body) => {
        assert.deepEqual(body.thinking, { type: 'enabled' })
        assert.equal(body.reasoning_effort, undefined)
        assert.equal(body.enable_thinking, undefined)
      },
    },
    {
      protocol: 'anthropic_messages',
      adapter: 'anthropic_adaptive',
      options: [
        { id: 'high', effort: 'high' },
        { id: 'off', enabled: false },
      ],
      selected: 'high',
      verify: (body) => {
        assert.deepEqual(body.thinking, { type: 'adaptive' })
        assert.deepEqual(body.output_config, { effort: 'high' })
      },
    },
    {
      protocol: 'anthropic_messages',
      adapter: 'messages_effort',
      options: [{ id: 'max', effort: 'max' }],
      selected: 'max',
      verify: (body) => {
        assert.deepEqual(body.thinking, { type: 'enabled' })
        assert.deepEqual(body.output_config, { effort: 'max' })
      },
    },
    {
      protocol: 'anthropic_messages',
      adapter: 'anthropic_budget',
      options: [
        { id: 'budget_4096', budget_tokens: 4096 },
        { id: 'off', enabled: false },
      ],
      selected: 'budget_4096',
      verify: (body) => {
        assert.deepEqual(body.thinking, { type: 'enabled', budget_tokens: 4096 })
        assert.equal(body.output_config, undefined)
      },
    },
    {
      protocol: 'chat_completions',
      adapter: 'chat_toggle',
      options: [
        { id: 'on', enabled: true },
        { id: 'off', enabled: false },
      ],
      selected: 'on',
      verify: (body) => {
        assert.equal(body.enable_thinking, true)
        assert.equal(body.reasoning_effort, undefined)
      },
    },
  ]
  for (const item of cases) {
    const configured = await save({
      ...base,
      protocol: item.protocol,
      model: `fixture-${item.adapter}`,
      reasoning_default: item.selected,
      reasoning_capability: { source: 'manual', adapter: item.adapter, options: item.options },
    })
    item.verify((await create(configured.model)).body)
  }
  // Exercise the actual settings form: the dialect must survive independently
  // of its default selection, and a relay gets editable, clearly labeled presets.
  await page.getByRole('link', { name: '模型', exact: true }).click()
  await page.getByLabel('API 基础地址').fill(baseInput.base_url)
  await page.getByLabel('模型名称', { exact: true }).fill('fixture-ui-reasoning')
  await page.getByLabel('API key', { exact: true }).fill(baseInput.api_key)
  await expect(page.getByLabel('思考设置').locator('option')).toHaveCount(4)
  await expect(
    page.getByText('使用默认预设，可在高级设置中调整；具体选项以服务支持为准。'),
  ).toBeVisible()
  await page.locator('.model-advanced > summary').click()
  await page.getByRole('button', { name: '自定义当前选项', exact: true }).click()
  await expect(page.getByLabel('思考选项 3 发送值', { exact: true })).toHaveValue('high')
  await page.getByLabel('思考参数模板').selectOption('deepseek')
  await expect(page.getByLabel('思考设置').locator('option')).toHaveCount(5)
  await expect(page.getByLabel('思考设置')).toHaveValue('')
  await page.getByLabel('思考选项 4 发送值', { exact: true }).fill('xhigh')
  const beforeFormSave = calls.length
  await page.getByRole('button', { name: '保存并使用', exact: true }).click()
  await expect(page.locator('.model-form [role=status]')).toContainText('模型已接入')
  assert.equal(calls.length, beforeFormSave)
  const formModel = await page.evaluate(async () =>
    (await window.harness.config()).models.find((model) => model.name === 'fixture-ui-reasoning'),
  )
  assert.equal(formModel.reasoning.default_effort, null)
  assert.equal(formModel.reasoning.capability.source, 'manual')
  assert.equal(formModel.reasoning.capability.adapter, 'deepseek')
  await page.screenshot({ path: 'test-results/desktop-model-reasoning.png' })
  const formRun = await create(formModel)
  assert.equal(formRun.body.thinking, undefined)
  body = await reply(formRun.run.id, formModel, 'max')
  assert.deepEqual(body.thinking, { type: 'enabled' })
  assert.equal(body.reasoning_effort, 'xhigh')
  return {
    id: started.run.id,
    model_id: automatic.model.id,
    mapped_id: mapped.model.id,
    preset_id: unknown.model.id,
    preset_run_id: presetRun.run.id,
  }
}

export async function verifyReasoningCapabilitiesAfterRestart(page, saved, settled, calls) {
  const config = await page.evaluate(() => window.harness.config())
  const model = config.models.find((model) => model.id === saved.model_id)
  assert.deepEqual(model.reasoning.supported_efforts, ['off', 'low', 'high', 'max'])
  assert.equal(model.reasoning.capability.source, 'metadata')
  const mapped = config.models.find((model) => model.id === saved.mapped_id)
  assert.equal(mapped.reasoning.default_effort, 'careful')
  assert.equal(mapped.reasoning.capability.options[1].label, '仔细思考')
  const preset = config.models.find((model) => model.id === saved.preset_id)
  assert.equal(preset.reasoning.capability.source, 'preset')
  assert.deepEqual(preset.reasoning.supported_efforts, ['low', 'medium', 'high'])
  const presetRun = await page.evaluate((id) => window.harness.getRun(id), saved.preset_run_id)
  assert.equal(presetRun.reasoning_effort, 'high')
  await page.evaluate(
    ({ id, model_id }) => window.harness.updateRunModel(id, { model_id, reasoning_effort: 'high' }),
    saved,
  )
  const before = calls.length
  await page.evaluate((id) => window.harness.submitInput(id, 'Continue after restart.'), saved.id)
  await settled(page, saved.id)
  const body = calls.slice(before).find((call) => !call.summary).body
  assert.deepEqual(body.thinking, { type: 'enabled' })
  assert.equal(body.reasoning_effort, 'high')
}
