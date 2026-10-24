import assert from 'node:assert/strict'
import { execFile, fork } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { promisify } from 'node:util'

const execute = promisify(execFile)

function isAlive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if (error.code === 'ESRCH') return false
    throw error
  }
}

async function stopProcess(pid) {
  if (!isAlive(pid)) return false
  if (process.platform === 'win32') {
    try {
      await execute('taskkill.exe', ['/PID', String(pid), '/T', '/F'], {
        windowsHide: true,
        timeout: 5_000,
      })
    } catch (error) {
      if (isAlive(pid)) throw error
      return false
    }
  } else {
    try {
      process.kill(pid, 'SIGKILL')
    } catch (error) {
      if (error.code !== 'ESRCH') throw error
      return false
    }
  }
  return true
}

async function bounded(promise, timeout, label) {
  let timer
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out`)), Math.max(1, timeout))
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

// Electron's Windows cmd wrapper can exit while Playwright retains a pending
// CDP close chain. Isolate those test-only handles from the result/exit owner.
export async function runPackagedWorker({
  workerPath,
  artifacts,
  environment = process.env,
  timeoutMs = 600_000,
  shutdownGraceMs = 5_000,
}) {
  await mkdir(artifacts, { recursive: true })
  const deadline = Date.now() + timeoutMs
  const child = fork(workerPath, [], {
    env: { ...environment, OPENHARNESS_E2E_ARTIFACTS: artifacts },
    execArgv: [],
    stdio: ['ignore', 'inherit', 'inherit', 'ipc'],
    windowsHide: true,
  })
  const started = new Set()
  const completed = Promise.withResolvers()
  const exited = Promise.withResolvers()
  const failed = Promise.withResolvers()
  let protocolError
  let completion
  let exit
  child.on('message', (message) => {
    try {
      if (message?.kind === 'application-started' && !completion) {
        for (const pid of [message.pid, message.wrapperPid]) {
          assert.ok(
            Number.isSafeInteger(pid) && pid > 0 && pid !== process.pid && pid !== child.pid,
          )
          started.add(pid)
        }
      } else if (message?.kind === 'completed' && !completion) {
        assert.equal(typeof message.passed, 'boolean')
        assert.ok(Number.isSafeInteger(message.exitCode) && message.exitCode >= 0)
        completion = message
        completed.resolve(message)
      } else throw new Error('Unexpected packaged worker acknowledgement')
    } catch (error) {
      protocolError = error
      failed.reject(error)
    }
  })
  child.once('error', (error) => failed.reject(error))
  child.once('exit', (code, signal) => {
    exit = { code, signal }
    exited.resolve(exit)
  })
  const result = {
    passed: false,
    forcedWorkerExit: false,
    applicationPids: [],
    workerExitCode: null,
  }
  try {
    await bounded(
      Promise.race([
        completed.promise,
        failed.promise,
        exited.promise.then(() => {
          throw new Error('Packaged worker exited without completion')
        }),
      ]),
      deadline - Date.now(),
      'Packaged worker completion',
    )
    const saved = JSON.parse(await readFile(join(artifacts, 'report.json'), 'utf8'))
    assert.equal(saved.passed, completion.passed, 'saved report and completion must agree')
    assert.equal(
      completion.passed,
      true,
      saved.error ?? saved.cleanupError ?? 'Packaged worker reported failure',
    )
    assert.equal(completion.exitCode, 0, 'worker must report a successful outcome')
    assert.ok(started.size > 0, 'worker must report the actual application process')
    for (const pid of started)
      assert.equal(isAlive(pid), false, `Application or wrapper PID ${pid} is still running`)
    const remaining = Math.min(shutdownGraceMs, deadline - Date.now())
    let outcome
    try {
      outcome = await bounded(exited.promise, remaining, 'Packaged worker exit')
    } catch {
      if (Date.now() >= deadline) throw new Error('Packaged worker exceeded the total deadline')
      // Only the completed, independently verified test worker may need force.
      // A live application or a nonzero worker exit is never turned into a pass.
      for (const pid of started)
        assert.equal(isAlive(pid), false, `Application or wrapper PID ${pid} restarted`)
      if (exit) outcome = exit
      else {
        console.log(
          '[packaged] Application processes exited; cleaning residual test worker handles',
        )
        result.forcedWorkerExit = await stopProcess(child.pid)
        outcome = await bounded(exited.promise, 5_000, 'Packaged worker termination')
      }
    }
    if (protocolError) throw protocolError
    if (!result.forcedWorkerExit)
      assert.equal(outcome.code, 0, 'packaged worker exited with failure')
    result.passed = true
  } catch (error) {
    result.error = error instanceof Error ? error.stack : String(error)
    console.error(result.error)
  } finally {
    for (const pid of [...started, child.pid]) {
      try {
        if (isAlive(pid)) await stopProcess(pid)
      } catch (error) {
        result.passed = false
        ;(result.cleanupErrors ??= []).push(String(error))
      }
    }
    if (child.pid && !exit) {
      try {
        await bounded(exited.promise, 5_000, 'Packaged worker cleanup')
      } catch (error) {
        result.passed = false
        ;(result.cleanupErrors ??= []).push(String(error))
      }
    }
    result.workerExitCode = exit?.code ?? null
    result.applicationPids = [...started]
    await writeFile(join(artifacts, 'supervisor.json'), JSON.stringify(result, null, 2))
  }
  return result
}
