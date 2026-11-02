import assert from 'node:assert/strict'
import { electronFixtureArgs } from './fixture-environment.mjs'
import { createServer } from 'node:http'
import { mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

if (!process.env.OPENHARNESS_E2E_TEMP_ROOT)
  throw new Error('Run through regression.mjs startup to isolate credentials')
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-startup-e2e-'))
const requests = []
const provider = createServer((request, response) => {
  requests.push(request.method)
  response.writeHead(200, { 'Content-Type': 'application/json' })
  response.end(JSON.stringify({ data: [] }))
})
await new Promise((resolve) => provider.listen(0, '127.0.0.1', resolve))
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application
async function launch(gated = false) {
  application = await _electron.launch({
    executablePath: require('electron'),
    args: electronFixtureArgs(),
    env: { ...env, OPENHARNESS_E2E_UNLOCK_GATE: gated ? '1' : '0' },
    timeout: 30_000,
  })
  return application.firstWindow()
}
try {
  let page = await launch()
  assert.equal(await application.evaluate(() => globalThis.__credentialFixtureActive), true)
  await expect
    .poll(() => page.evaluate(async () => (await window.harness.config()).model_restore_status))
    .toBe('ready')
  const saved = await page.evaluate(async (base_url) => {
    const model = await window.harness.configureModel({
      protocol: 'chat_completions',
      base_url,
      model: 'startup-fixture',
      api_key: 'synthetic-startup-key',
      context_window: 64000,
      max_output_tokens: 2048,
      reasoning_mode: 'off',
      reasoning_effort: 'medium',
    })
    const run = await window.harness.createRun({
      prompt: 'Startup history fixture',
      model_id: 'local-demo',
    })
    await window.harness.cancelRun(run.id)
    return { model: model.model.id, run: run.id, persisted: model.persisted }
  }, `http://127.0.0.1:${provider.address().port}/v1`)
  assert(saved.persisted)
  await application.close()
  application = undefined
  const damagedRunId = 'run-startup-damaged-fixture'
  const damagedPath = join(directory, 'runtime', 'runs', `${damagedRunId}.json`)
  const damagedContents = '{"fixture":"intentionally invalid, non-sensitive snapshot"'
  await writeFile(damagedPath, damagedContents, 'utf8')
  page = await launch(true)
  await expect
    .poll(() => application.evaluate(() => typeof globalThis.__fixtureUnlock))
    .toBe('function')
  // This call would hang if either the main event loop or bridge still awaited keys.
  const start = Date.now()
  const observed = await page.evaluate(
    async (id) => ({
      config: await window.harness.config(),
      runs: await window.harness.listRuns(),
      run: await window.harness.getRun(id),
    }),
    saved.run,
  )
  assert.equal(observed.config.model_restore_status, 'loading')
  assert(observed.runs.some((run) => run.id === saved.run))
  assert.equal(observed.run.id, saved.run)
  assert(Date.now() - start < 5_000, 'history must be available before credential unlock')
  const damagedIssue = observed.config.recovery_issues?.find(
    (issue) => issue.run_id === damagedRunId,
  )
  assert(
    damagedIssue,
    'a damaged snapshot must be reported without blocking healthy history or credential unlock',
  )
  assert.equal(await realpath(damagedIssue.source_path), await realpath(damagedPath))
  assert(!observed.runs.some((run) => run.id === damagedRunId))
  await page.getByRole('button', { name: '部分会话未恢复', exact: true }).click()
  await expect(page.getByRole('heading', { name: '设置', exact: true })).toBeVisible()
  const recovery = page.locator('.recovery-issues')
  await expect(recovery.getByRole('heading', { name: '部分会话未恢复' })).toBeVisible()
  await recovery.locator('summary').filter({ hasText: '会话记录格式或标识不正确' }).click()
  await expect(recovery.getByText(damagedIssue.source_path, { exact: true })).toBeVisible()
  assert.equal(await readFile(damagedPath, 'utf8'), damagedContents)
  await page.evaluate(() => {
    window.location.hash = '/models'
  })
  await page.reload()
  await expect(page.getByRole('status')).toContainText('正在解锁已保存的模型')
  await application.evaluate(() => globalThis.__fixtureUnlock(false))
  await expect(page.getByRole('alert')).toContainText('本地历史仍可查看')
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await expect
    .poll(() => application.evaluate(() => typeof globalThis.__fixtureUnlock))
    .toBe('function')
  await application.evaluate(() => globalThis.__fixtureUnlock(true))
  await expect
    .poll(() => page.evaluate(async () => (await window.harness.config()).model_restore_status))
    .toBe('ready')
  const restored = await page.evaluate(() => window.harness.config())
  assert(restored.models.some((model) => model.id === saved.model))
  assert.equal(await readFile(damagedPath, 'utf8'), damagedContents)
  assert.equal(
    requests.filter((method) => method === 'POST').length,
    0,
    'restoration must not call a model or resume a task',
  )
  console.log(
    'PASS: healthy history and damaged-snapshot diagnostics remain accessible during credential unlock; damaged records are preserved and explicit retry restores models without task replay',
  )
} finally {
  await application?.close()
  provider.closeAllConnections()
  await new Promise((resolve) => provider.close(resolve))
  await rm(directory, { recursive: true, force: true })
}
