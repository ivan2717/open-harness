import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect } from '@playwright/test'

/** Playwright removes NODE_OPTIONS; Electron's explicit preload runs before app startup. */
export function electronFixtureArgs() {
  const fixture = resolve(dirname(fileURLToPath(import.meta.url)), 'credential-fixture.cjs')
  return [...(process.env.OPENHARNESS_E2E_TEMP_ROOT ? ['-r', fixture] : []), resolve('.')]
}

export async function waitForRestoredModels(page) {
  await expect
    .poll(async () => {
      const config = await page.evaluate(() => window.harness.config())
      return config.model_restore_status ?? 'ready'
    })
    .toBe('ready')
}
