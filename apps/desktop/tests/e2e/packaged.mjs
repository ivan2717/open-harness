import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { runPackagedWorker } from './packaged-process.mjs'

if (process.platform === 'win32') {
  const result = await runPackagedWorker({
    workerPath: fileURLToPath(new URL('./packaged-worker.mjs', import.meta.url)),
    artifacts: resolve(process.env.OPENHARNESS_E2E_ARTIFACTS ?? 'test-results/packaged'),
  })
  process.exitCode = result.passed ? 0 : 1
} else {
  await import('./packaged-worker.mjs')
}
