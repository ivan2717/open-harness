import assert from 'node:assert/strict'
import { access, mkdir, mkdtemp, realpath, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

const executable = process.env.OPENHARNESS_E2E_EXECUTABLE
if (!executable || !isAbsolute(executable)) {
  throw new Error('OPENHARNESS_E2E_EXECUTABLE must identify an installed application executable')
}
await access(executable)

// A fresh profile exercises the installed bundle without reading a real user's
// credentials, changing their projects, or calling an external model provider.
const directory = await mkdtemp(join(tmpdir(), 'openharness-packaged-'))
const artifacts = resolve(process.env.OPENHARNESS_E2E_ARTIFACTS ?? 'test-results/packaged')
await mkdir(artifacts, { recursive: true })
const environment = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete environment.ELECTRON_RUN_AS_NODE
delete environment.ELECTRON_RENDERER_URL
const rendererErrors = []
const logs = []
const report = { passed: false, executable, artifacts, launches: [] }
let application
let page

function record(source, value) {
  // Bound diagnostic output even if a broken process repeatedly logs errors.
  const entry = `[${source}] ${String(value).slice(0, 16_384)}`
  if (logs.length < 500) logs.push(entry)
}

async function launch() {
  application = await _electron.launch({
    executablePath: executable,
    args: [],
    env: environment,
    timeout: 45_000,
  })
  const child = application.process()
  child.stdout?.on('data', (chunk) => record('stdout', chunk))
  child.stderr?.on('data', (chunk) => record('stderr', chunk))
  application.on('console', (message) => record('main', message.text()))
  const runtime = await application.evaluate(({ app }) => ({
    packaged: app.isPackaged,
    version: app.getVersion(),
    resources: process.resourcesPath,
    profile: app.getPath('userData'),
    executable: process.execPath,
    platform: process.platform,
    arch: process.arch,
  }))
  report.launches.push(runtime)
  assert.equal(runtime.packaged, true, 'the smoke test must run the installed application')
  assert.equal(await realpath(runtime.executable), await realpath(executable))
  assert.equal(await realpath(runtime.profile), await realpath(directory))
  const backend = join(runtime.resources, 'bin', runtime.platform === 'win32' ? 'api.exe' : 'api')
  assert.equal((await stat(backend)).isFile(), true, 'the installed Rust backend must exist')
  assert.equal((await stat(join(runtime.resources, 'app.asar'))).isFile(), true)
  await access(join(runtime.resources, 'browser-runtime', 'index.js'))
  await access(join(runtime.resources, 'plugins', 'browser-use', 'server.mjs'))
  await access(join(runtime.resources, 'plugins', 'chrome-use', 'extension', 'manifest.json'))

  page = await application.firstWindow()
  page.setDefaultTimeout(15_000)
  page.on('pageerror', (error) => rendererErrors.push(error.message))
  page.on('console', (message) => record(`renderer:${message.type()}`, message.text()))
  await expect(page.locator('body')).toBeVisible()
  await page.waitForFunction(() => typeof window.harness?.config === 'function')
  const config = await page.evaluate(() => window.harness.config())
  assert.ok(config.models.some((model) => model.id === 'local-demo' && model.available))
  assert.equal(config.model_config_error, undefined)
  assert.deepEqual(
    await page.evaluate(() => ({
      node: typeof globalThis.process,
      require: typeof globalThis.require,
    })),
    { node: 'undefined', require: 'undefined' },
    'the packaged renderer must retain Node isolation',
  )
  return page
}

async function close() {
  if (!application) return
  await application.close()
  application = undefined
  page = undefined
}

try {
  await launch()
  assert.deepEqual(await page.evaluate(() => window.harness.listRuns()), [])
  const prompt = 'Verify the installed application, offline execution and conversation persistence.'
  const run = await page.evaluate(
    (prompt) => window.harness.createRun({ prompt, model_id: 'local-demo' }),
    prompt,
  )
  report.runId = run.id
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, run.id)
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, run.id), {
      timeout: 90_000,
      intervals: [250, 500, 1_000],
    })
    .toBe('completed')
  const completed = await page.evaluate((id) => window.harness.getRun(id), run.id)
  assert.equal(completed.model_id, 'local-demo')
  assert.ok(completed.events.some((event) => event.type === 'tool_call_succeeded'))
  assert.ok(completed.context.compaction_count > 0, 'the real context manager must compact')
  assert.ok(
    completed.messages.some(
      (message) => message.role === 'assistant' && message.status === 'complete',
    ),
  )
  await expect(page.getByText(prompt, { exact: true }).first()).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'completed.png'), fullPage: true })
  report.events = completed.events.length
  report.compactions = completed.context.compaction_count
  await close()

  await launch()
  const restored = await page.evaluate((id) => window.harness.getRun(id), run.id)
  assert.equal(restored.status, 'completed')
  assert.equal(restored.prompt, prompt)
  assert.deepEqual(
    restored.events,
    completed.events,
    'committed events must survive an app restart',
  )
  assert.deepEqual(restored.messages, completed.messages)
  assert.equal(restored.context.compaction_count, completed.context.compaction_count)
  assert.ok(
    (await page.evaluate(() => window.harness.listRuns())).some((item) => item.id === run.id),
  )
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, run.id)
  await expect(page.getByText(prompt, { exact: true }).first()).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'restored.png'), fullPage: true })
  assert.deepEqual(rendererErrors, [], 'the packaged renderer must have no uncaught exceptions')
  await close()
  report.passed = true
  console.log(JSON.stringify(report, null, 2))
} catch (error) {
  report.error = error instanceof Error ? error.stack : String(error)
  if (page && !page.isClosed()) {
    await page.screenshot({ path: join(artifacts, 'failure.png'), fullPage: true }).catch(() => {})
  }
  throw error
} finally {
  try {
    await close()
  } finally {
    await writeFile(
      join(artifacts, 'report.json'),
      JSON.stringify({ ...report, rendererErrors }, null, 2),
    )
    await writeFile(join(artifacts, 'application.log'), logs.join('\n'))
    await rm(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 })
  }
}
