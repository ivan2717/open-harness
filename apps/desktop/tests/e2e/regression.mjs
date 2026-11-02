import { spawn } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { stopTestProcessTree } from './process-cleanup.mjs'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const suites = [
  'startup',
  'steering',
  'conversation-activity',
  'agent-panel',
  'model-editing',
  'file-references',
  'image-references',
  'history',
]
const requested = process.argv.slice(2)
if (requested.some((suite) => !suites.includes(suite)))
  throw new Error('Unknown desktop regression suite')
const selected = requested.length ? suites.filter((suite) => requested.includes(suite)) : suites
const root = await mkdtemp(join(tmpdir(), 'openharness-regression-'))
const results = []
let stopActive
let interrupted = false
function terminate() {
  interrupted = true
  stopActive?.()
}
process.on('SIGINT', terminate)
process.on('SIGTERM', terminate)
try {
  for (const suite of selected) {
    if (interrupted) break
    const directory = join(root, suite)
    await mkdir(directory, { recursive: true })
    const env = { ...process.env }
    // The fixtures configure loopback model servers. Never inherit real provider
    // credentials, packaged-app targets, user data or browser bridge settings.
    for (const name of Object.keys(env)) {
      if (
        /^(OPENHARNESS_|OPENAI_|ANTHROPIC_|DEEPSEEK_|DASHSCOPE_|MOONSHOT_|GEMINI_|GOOGLE_API_|XAI_)/.test(
          name,
        ) ||
        /(?:API_KEY|ACCESS_TOKEN|SECRET_KEY)$/.test(name)
      )
        delete env[name]
    }
    delete env.ELECTRON_RUN_AS_NODE
    Object.assign(env, {
      TMPDIR: directory,
      TMP: directory,
      TEMP: directory,
      OPENHARNESS_E2E_TEMP_ROOT: directory,
      OPENHARNESS_E2E_CIPHER_KEY: randomBytes(32).toString('hex'),
      NODE_OPTIONS: '',
    })
    const start = Date.now()
    console.log(`\nDesktop regression: ${suite}`)
    const result = await new Promise((resolve) => {
      const child = spawn(process.execPath, [join(desktop, 'tests/e2e', `${suite}.mjs`)], {
        cwd: desktop,
        env,
        stdio: 'inherit',
        windowsHide: true,
        detached: process.platform !== 'win32',
      })
      let timedOut = false
      let cleanup
      let finished = false
      const stop = () => {
        cleanup ??= stopTestProcessTree(child).catch((error) => {
          console.error(`Could not clean up ${suite} process tree: ${error.message}`)
          child.kill('SIGKILL')
          return error.message
        })
      }
      stopActive = stop
      const deadline = setTimeout(() => {
        timedOut = true
        stop()
      }, 5 * 60_000)
      const finish = async (code, signal, error) => {
        if (finished) return
        finished = true
        clearTimeout(deadline)
        const cleanupError = await cleanup
        stopActive = undefined
        resolve({
          suite,
          code,
          signal,
          timedOut,
          ...(error || cleanupError ? { error: error || cleanupError } : {}),
          durationMs: Date.now() - start,
        })
      }
      child.once('error', (error) => finish(null, null, error.message))
      child.once('exit', (code, signal) => finish(code, signal))
    })
    results.push(result)
    await rm(directory, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 })
    if (result.code !== 0 || result.timedOut || interrupted) break
  }
} finally {
  process.removeListener('SIGINT', terminate)
  process.removeListener('SIGTERM', terminate)
  await mkdir(join(desktop, 'test-results'), { recursive: true })
  await writeFile(
    join(desktop, 'test-results/desktop-regression-report.json'),
    JSON.stringify(
      {
        passed:
          !interrupted &&
          results.length === selected.length &&
          results.every((result) => result.code === 0 && !result.timedOut),
        platform: process.platform,
        credentialStore: 'isolated synthetic cipher; OS keychain consent is not tested',
        results,
      },
      null,
      2,
    ),
  )
  await rm(root, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 })
}
if (
  interrupted ||
  results.length !== selected.length ||
  results.some((result) => result.code !== 0 || result.timedOut)
)
  process.exitCode = 1
