import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { access, mkdir, mkdtemp, realpath, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { _electron, expect } from '@playwright/test'
import { verifyWindowsChrome } from './windows-chrome.mjs'

const executable = process.env.OPENHARNESS_E2E_EXECUTABLE
if (!executable || !isAbsolute(executable)) {
  throw new Error('OPENHARNESS_E2E_EXECUTABLE must identify an installed application executable')
}
await access(executable)

// A fresh profile exercises the installed bundle without reading a real user's
// credentials, changing their projects, or calling an external model provider.
const profilePrefix =
  process.platform === 'win32' ? 'Open Harness 中文 & %PATH% packaged-' : 'openharness-packaged-'
const directory = await mkdtemp(join(tmpdir(), profilePrefix))
const artifacts = resolve(process.env.OPENHARNESS_E2E_ARTIFACTS ?? 'test-results/packaged')
await mkdir(artifacts, { recursive: true })
const environment = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete environment.ELECTRON_RUN_AS_NODE
delete environment.ELECTRON_RENDERER_URL
const rendererErrors = []
const logs = []
const report = { passed: false, executable, artifacts, launches: [] }
let application
let applicationPid
let page
const execute = promisify(execFile)

async function within(label, operation, timeout) {
  let timer
  try {
    return await Promise.race([
      operation(),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${timeout} ms`)),
          timeout,
        )
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

async function saveDiagnostics() {
  await writeFile(
    join(artifacts, 'report.json'),
    JSON.stringify({ ...report, rendererErrors }, null, 2),
  )
  await writeFile(join(artifacts, 'application.log'), logs.join('\n'))
}

async function checkpoint(stage) {
  report.stage = stage
  console.log(`[packaged] ${stage}`)
  await saveDiagnostics()
}

function record(source, value) {
  // Bound diagnostic output even if a broken process repeatedly logs errors.
  const entry = `[${source}] ${String(value).slice(0, 16_384)}`
  if (logs.length < 500) logs.push(entry)
}

async function launch() {
  applicationPid = undefined
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
    pid: process.pid,
  }))
  assert.ok(Number.isSafeInteger(runtime.pid) && runtime.pid > 0)
  applicationPid = runtime.pid
  record('process', `Electron PID ${runtime.pid}; Playwright wrapper PID ${child.pid}`)
  process.send?.({ kind: 'application-started', pid: runtime.pid, wrapperPid: child.pid })
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
  const current = application
  const pid = applicationPid
  application = undefined
  applicationPid = undefined
  page = undefined
  const child = current.process()
  let closeSettled = false
  const closing = current.close().finally(() => {
    closeSettled = true
  })
  try {
    await within('Installed application shutdown', () => closing, 15_000)
  } catch (error) {
    if (process.platform === 'win32' && Number.isSafeInteger(pid) && pid > 0) {
      let alive
      try {
        process.kill(pid, 0)
        alive = true
      } catch (checkError) {
        if (checkError.code === 'ESRCH') alive = false
        else record('cleanup', `Unable to verify Electron PID ${pid}: ${checkError.message}`)
      }
      record(
        'cleanup',
        `Electron PID ${pid} alive=${alive}; wrapper PID ${child.pid}, exitCode=${child.exitCode}`,
      )
      if (alive === false && !closeSettled) {
        // Playwright uses a cmd.exe wrapper on Windows. Inherited pipe handles
        // can delay its close event even after the real Electron process exits.
        for (const stream of child.stdio) stream?.destroy()
        try {
          await within('Playwright wrapper pipe shutdown', () => closing, 5_000)
          record('cleanup', `Recovered wrapper pipe shutdown after Electron PID ${pid} exited`)
          return
        } catch (recoveryError) {
          record('cleanup', recoveryError.message)
          if (child.exitCode === 0 && process.send) {
            // The supervisor independently checks both OS PIDs, the saved
            // report and worker outcome before allowing this isolated worker
            // to be terminated if Playwright's CDP bookkeeping stays pending.
            const diagnostic = {
              pid,
              wrapperPid: child.pid,
              wrapperExitCode: child.exitCode,
              playwrightClosePending: true,
            }
            ;(report.shutdowns ??= []).push(diagnostic)
            record(
              'cleanup',
              'Application process exited; deferring stale Playwright handles to the test supervisor',
            )
            return
          }
        }
      } else if (alive === true) {
        await execute('taskkill.exe', ['/PID', String(pid), '/T', '/F'], {
          windowsHide: true,
          timeout: 10_000,
        }).catch((failure) => record('cleanup', failure.message))
        for (const stream of child.stdio) stream?.destroy()
        await within('Playwright wrapper cleanup after termination', () => closing, 5_000).catch(
          (failure) => record('cleanup', failure.message),
        )
      }
    } else if (
      process.platform !== 'win32' &&
      child.exitCode === null &&
      child.signalCode === null
    ) {
      child.kill('SIGKILL')
    }
    throw error
  }
}

try {
  await checkpoint('launch installed application')
  await within('Installed application startup', launch, 60_000)
  if (process.platform === 'win32') {
    await checkpoint('verify Windows Chrome connection')
    report.chrome = await verifyWindowsChrome({
      page,
      runtime: report.launches.at(-1),
      directory,
      artifacts,
    })
  }
  await checkpoint('run offline task')
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
  await checkpoint('close before restart')
  await close()

  await checkpoint('restart installed application')
  await within('Installed application restart', launch, 60_000)
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
  await checkpoint('close after restart')
  await close()
  report.passed = true
  console.log(JSON.stringify(report, null, 2))
} catch (error) {
  report.error = error instanceof Error ? error.stack : String(error)
  process.exitCode = 1
  console.error(report.error)
  // Persist the original failure before screenshots or process cleanup can hang.
  await saveDiagnostics()
  if (page && !page.isClosed()) {
    await within(
      'Failure screenshot',
      () => page.screenshot({ path: join(artifacts, 'failure.png'), fullPage: true }),
      5_000,
    ).catch(() => {})
  }
} finally {
  try {
    await close()
  } catch (error) {
    report.passed = false
    report.cleanupError = error instanceof Error ? error.stack : String(error)
    process.exitCode = 1
    console.error(report.cleanupError)
  } finally {
    await saveDiagnostics()
    try {
      await rm(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 })
    } catch (error) {
      report.passed = false
      report.directoryCleanupError = error instanceof Error ? error.message : String(error)
      process.exitCode = 1
      console.error(report.directoryCleanupError)
      await saveDiagnostics()
    }
  }
}

// Acknowledge only after restart verification, all cleanup and final diagnostics.
if (process.send) {
  await new Promise((resolve, reject) => {
    process.send(
      { kind: 'completed', passed: report.passed, exitCode: process.exitCode ?? 0 },
      (error) => (error ? reject(error) : resolve()),
    )
  })
  process.disconnect()
}
