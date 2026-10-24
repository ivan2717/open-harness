import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { test } from 'vitest'
import { runPackagedWorker } from './packaged-process.mjs'

const workerSource = String.raw`
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const directory = process.env.OPENHARNESS_WORKER_FIXTURE
const mode = process.env.OPENHARNESS_WORKER_MODE
const send = (message) => new Promise((resolve, reject) => {
  process.send(message, (error) => error ? reject(error) : resolve())
})
const childSource = 'const fs = require("node:fs"); setInterval(() => { if (fs.existsSync(process.env.OPENHARNESS_FIXTURE_STOP)) process.exit(0) }, 40)'
const application = spawn(process.execPath, ['-e', childSource], {
  detached: true,
  stdio: 'ignore',
  env: { ...process.env, OPENHARNESS_FIXTURE_STOP: join(directory, 'stop') },
})
await once(application, 'spawn')
await writeFile(join(directory, 'owned.json'), JSON.stringify({
  workerPid: process.pid,
  applicationPid: application.pid,
}))
await send({ kind: 'application-started', pid: application.pid, wrapperPid: application.pid })

if (mode !== 'application-alive' && mode !== 'timeout') {
  const exited = once(application, 'exit')
  application.kill('SIGTERM')
  await exited
}
application.unref()

if (mode === 'timeout') {
  setInterval(() => {}, 1_000)
} else {
  await writeFile(join(directory, 'report.json'), JSON.stringify({
    passed: mode !== 'report-mismatch',
  }))
  if (mode !== 'missing-completed') {
    await send({ kind: 'completed', passed: true, exitCode: 0 })
  }
  process.disconnect()
  if (mode === 'residual-handle' || mode === 'application-alive') {
    setInterval(() => {}, 1_000)
  } else if (mode === 'nonzero-exit') {
    process.exitCode = 7
  }
}
`

function isAlive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if (error.code === 'ESRCH') return false
    throw error
  }
}

async function waitUntilExited(pid) {
  const deadline = Date.now() + 3_000
  while (isAlive(pid) && Date.now() < deadline) await delay(40)
  assert.equal(isAlive(pid), false, `Fixture process ${pid} must have exited`)
}

async function fixture(mode, check) {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-worker-test-'))
  const workerPath = join(directory, 'worker.mjs')
  await writeFile(workerPath, workerSource)
  let owned
  try {
    const result = await runPackagedWorker({
      workerPath,
      artifacts: directory,
      environment: {
        ...process.env,
        OPENHARNESS_WORKER_FIXTURE: directory,
        OPENHARNESS_WORKER_MODE: mode,
      },
      timeoutMs: mode === 'timeout' ? 2_000 : 10_000,
      shutdownGraceMs: 1_000,
    })
    owned = JSON.parse(await readFile(join(directory, 'owned.json'), 'utf8'))
    await check(result, owned)
    await waitUntilExited(owned.applicationPid)
    await waitUntilExited(owned.workerPid)
  } finally {
    // This fallback only signals our fixture through its private stop file. It
    // never searches for or terminates real browsers or installed applications.
    await writeFile(join(directory, 'stop'), '')
    owned ??= await readFile(join(directory, 'owned.json'), 'utf8').then(JSON.parse, () => null)
    if (owned) await waitUntilExited(owned.applicationPid)
    await rm(directory, { recursive: true, force: true })
  }
}

test(
  'packaged worker completes normally after its application exits',
  () =>
    fixture('normal', (result) => {
      assert.equal(result.passed, true)
      assert.equal(result.workerExitCode, 0)
      assert.equal(result.forcedWorkerExit, false)
    }),
  20_000,
)

test(
  'a successful worker with a residual handle is cleaned up only after its application exits',
  () =>
    fixture('residual-handle', (result) => {
      assert.equal(result.passed, true)
      assert.equal(result.forcedWorkerExit, true)
    }),
  20_000,
)

test(
  'a success report cannot hide a live application, which is cleaned up',
  () =>
    fixture('application-alive', (result) => {
      assert.equal(result.passed, false)
      assert.ok(result.error)
    }),
  20_000,
)

test(
  'a nonzero worker exit fails even after a successful completed message',
  () =>
    fixture('nonzero-exit', (result) => {
      assert.equal(result.passed, false)
      assert.equal(result.workerExitCode, 7)
      assert.ok(result.error)
    }),
  20_000,
)

test(
  'a report and zero exit cannot replace the completed message',
  () =>
    fixture('missing-completed', (result) => {
      assert.equal(result.passed, false)
      assert.equal(result.workerExitCode, 0)
      assert.ok(result.error)
    }),
  20_000,
)

test(
  'an unfinished worker times out and its application is cleaned up',
  () =>
    fixture('timeout', (result) => {
      assert.equal(result.passed, false)
      assert.ok(result.error)
    }),
  20_000,
)

test(
  'a report that contradicts the completed message fails',
  () =>
    fixture('report-mismatch', (result) => {
      assert.equal(result.passed, false)
      assert.ok(result.error)
    }),
  20_000,
)
