import assert from 'node:assert/strict'
import { expect } from '@playwright/test'

/** Runs through the actual renderer/preload/backend against the local fixture provider. */
export async function exerciseRunModelSettings(page, { baseInput, calls, settled }) {
  const inputs = [
    { protocol: 'responses', model: 'switch-responses', context_window: 64000 },
    { protocol: 'anthropic_messages', model: 'switch-messages', context_window: 32000 },
    { protocol: 'chat_completions', model: 'switch-deepseek', context_window: 128000 },
  ].map((input) => ({
    ...baseInput,
    ...input,
    max_output_tokens: 16384,
    reasoning_mode: input.protocol === 'chat_completions' ? 'deepseek' : 'standard',
    reasoning_effort: 'medium',
  }))
  const models = []
  for (const input of inputs) {
    const configured = await page.evaluate((input) => window.harness.configureModel(input), input)
    models.push(configured.model)
  }
  await page.evaluate(() => {
    window.location.hash = '/'
  })
  await page.reload()
  const picker = page.getByRole('button', { name: '选择模型', exact: true })
  const draft = page.getByRole('textbox', { name: '任务描述', exact: true })
  async function chooseModel(model) {
    await picker.click()
    await page
      .getByRole('group', { name: '模型', exact: true })
      .getByRole('menuitemradio', { name: model.name, exact: true })
      .click()
    await expect(picker).toContainText(model.name)
  }
  async function chooseEffort(label, control = '思考强度') {
    await picker.click()
    await page
      .getByRole('group', { name: control, exact: true })
      .getByRole('menuitemradio', { name: label, exact: true })
      .click()
  }
  await chooseModel(models[0])
  await draft.fill('Remember PROJECT-CEDAR. Start the switch verification.')
  await chooseEffort('低')
  await expect(draft).toHaveValue('Remember PROJECT-CEDAR. Start the switch verification.')
  const beforeCreate = calls.length
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect(page).toHaveURL(/#\/runs\/[^/?]+$/)
  const id = await page.evaluate(() => window.location.hash.split('/')[2])
  const original = await settled(page, id)
  assert.equal(original.reasoning_effort, 'low')
  assert.equal(calls.slice(beforeCreate).find((call) => !call.summary).body.reasoning.effort, 'low')
  await expect(picker).toBeEnabled()

  const keptDraft = 'Keep my draft while selecting a different model.'
  await draft.fill(keptDraft)
  const beforeSwitch = calls.length
  await chooseModel(models[1])
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).model_id)
    .toBe(models[1].id)
  const switched = await page.evaluate((id) => window.harness.getRun(id), id)
  assert.equal(switched.reasoning_effort ?? null, null, 'model switch clears previous override')
  assert.deepEqual(switched.messages, original.messages, 'changing model preserves visible history')
  assert.deepEqual(
    switched.events,
    original.events,
    'changing model does not rewrite the event log',
  )
  assert.equal(switched.context.window_tokens, 32000)
  assert.equal(calls.length, beforeSwitch, 'selecting a model makes no generation request')
  await expect(draft).toHaveValue(keptDraft)
  await chooseEffort(/8[,\s]?192 tokens/, '思考预算')
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).reasoning_effort)
    .toBe('high')
  await expect(draft).toHaveValue(keptDraft)
  const beforeMessages = calls.length
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length, { timeout: 20_000 }).toBeGreaterThan(beforeMessages)
  const continued = await settled(page, id)
  const messageCall = calls.slice(beforeMessages).find((call) => !call.summary)
  assert.equal(messageCall.path, '/v1/messages')
  assert.equal(messageCall.body.thinking.budget_tokens, 8192)
  assert(JSON.stringify(messageCall.body).includes('Remember PROJECT-CEDAR'))
  assert(continued.messages.length > original.messages.length)

  await chooseModel(models[2])
  await chooseEffort('低')
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).reasoning_effort)
    .toBe('low')
  const beforeDeepSeek = calls.length
  await draft.fill('Continue with the same project after changing protocol again.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length, { timeout: 20_000 }).toBeGreaterThan(beforeDeepSeek)
  const finalRun = await settled(page, id)
  const deepseekCall = calls.slice(beforeDeepSeek).find((call) => !call.summary)
  assert.equal(deepseekCall.path, '/v1/chat/completions')
  assert.deepEqual(deepseekCall.body.thinking, { type: 'enabled' })
  assert.equal(deepseekCall.body.reasoning_effort, 'low')
  assert.equal(finalRun.context.window_tokens, 128000)
  const config = await page.evaluate(() => window.harness.config())
  for (const model of models) {
    assert.equal(
      config.models.find((item) => item.id === model.id).reasoning.default_effort,
      'medium',
      'per-conversation effort must not change global configuration',
    )
  }
  await page.reload()
  await expect(picker).toContainText(models[2].name)
  await expect(picker).toContainText('低')
  return { id, model_id: models[2].id, inputs, reasoning_effort: 'low' }
}

export async function verifyRunModelSettingsAfterRestart(page, saved, settled, calls) {
  const available = await page.evaluate(() => window.harness.config())
  for (const input of saved.inputs) {
    // Also works on platforms whose test user cannot persist a system-encrypted secret.
    if (!available.models.some((model) => model.name === input.model && model.available))
      await page.evaluate((input) => window.harness.configureModel(input), input)
  }
  const restored = await page.evaluate((id) => window.harness.getRun(id), saved.id)
  assert.equal(restored.model_id, saved.model_id)
  assert.equal(restored.reasoning_effort, saved.reasoning_effort)
  const before = calls.length
  await page.evaluate(
    (id) => window.harness.submitInput(id, 'Confirm PROJECT-CEDAR after restart.'),
    saved.id,
  )
  await settled(page, saved.id)
  const sent = calls.slice(before).find((call) => !call.summary)
  assert.equal(sent.body.reasoning_effort, saved.reasoning_effort)
  assert.equal(sent.body.model, 'switch-deepseek')
}
