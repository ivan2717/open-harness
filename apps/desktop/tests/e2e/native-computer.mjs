import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { _electron } from '@playwright/test'

// Run through the packaged independent Computer Use app, never request permissions.
const executable =
  process.env.OPENHARNESS_E2E_EXECUTABLE ??
  resolve('release/mac-arm64/Open Harness.app/Contents/MacOS/Open Harness')
const directory = await mkdtemp(join(tmpdir(), 'openharness-native-desktop-test-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
let application
try {
  application = await _electron.launch({
    executablePath: executable,
    args: [],
    env,
    timeout: 30000,
  })
  const window = await application.firstWindow()
  await window.waitForLoadState('domcontentloaded')
  const report = await application.evaluate(
    async ({ app }, { script, artifacts }) => {
      const { spawn } = process.getBuiltinModule('child_process')
      const path = process.getBuiltinModule('path')
      const helper = path.join(
        process.resourcesPath,
        'plugins/computer-use/bin/Open Harness Computer Use.app',
      )
      const launched = {
        packaged: app.isPackaged,
        executable: process.execPath,
        helper_path: helper,
      }
      const child = spawn(process.execPath, [script], {
        env: {
          ...process.env,
          ELECTRON_RUN_AS_NODE: '1',
          OPENHARNESS_COMPUTER_APP: helper,
          OPENHARNESS_COMPUTER_TEST_ARTIFACTS: artifacts,
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      let stdout = '',
        stderr = ''
      child.stdout.on('data', (data) => {
        stdout += data.toString()
      })
      child.stderr.on('data', (data) => {
        stderr += data.toString()
      })
      return await new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          child.kill('SIGTERM')
          reject(new Error('Native GUI smoke timed out.'))
        }, 120000)
        child.once('error', (error) => {
          clearTimeout(timer)
          reject(error)
        })
        child.once('exit', (code, signal) => {
          clearTimeout(timer)
          resolve({ ...launched, code, signal, stdout, stderr })
        })
      })
    },
    { script: resolve('../../plugins/computer-use/tests/gui-smoke.mjs'), artifacts },
  )
  await writeFile(join(artifacts, 'native-computer-report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify(report, null, 2))
  assert.equal(report.packaged, true)
  assert.equal(report.code, 0, report.stderr || report.stdout)
  const result = JSON.parse(report.stdout.trim().split('\n').at(-1))
  assert.equal(result.status, 'passed', 'Blocked checks are not GUI success')
} finally {
  if (application) await application.close()
  await rm(directory, { recursive: true, force: true })
}
