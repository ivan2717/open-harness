import assert from 'node:assert/strict'
import { access, mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

// First exercise the real offline settings/IPC/plugin path with isolated app
// data. Then replace Chrome and copy IPC handlers to test the published-store UI
// states. Fixtures never register a host, open Chrome, write the real clipboard
// or request grants.
const require = createRequire(import.meta.url)
const executable = process.env.OPENHARNESS_E2E_EXECUTABLE
if (executable && !isAbsolute(executable))
  throw new Error('OPENHARNESS_E2E_EXECUTABLE must be an absolute executable path')
const directory = await mkdtemp(join(tmpdir(), 'openharness-control-setup-'))
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: directory }
delete env.ELECTRON_RUN_AS_NODE
delete env.ELECTRON_RENDERER_URL
let application
const errors = []
try {
  if (!executable) {
    // The development main process resolves target/debug/api. Reuse the existing
    // release binary through an isolated target directory, without changing the
    // checkout or requiring another Rust build. Callers can select a newer build.
    const binary = process.env.OPENHARNESS_E2E_API_BINARY ?? resolve('../../target/release/api')
    await access(binary)
    env.CARGO_TARGET_DIR = join(directory, 'target')
    await mkdir(join(env.CARGO_TARGET_DIR, 'debug'), { recursive: true })
    await symlink(binary, join(env.CARGO_TARGET_DIR, 'debug', 'api'))
  }
  application = await _electron.launch({
    executablePath: executable ?? require('electron'),
    args: executable ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await page.evaluate(() => {
    window.location.hash = '/extensions'
  })
  const chrome = page.getByRole('article', { name: '插件 Chrome', exact: true })
  const browser = page.getByRole('article', { name: '插件 独立浏览器', exact: true })
  const computerCard = page.getByRole('article', { name: '插件 电脑操作', exact: true })
  const expectSingleCards = async () => {
    for (const name of ['Chrome', '独立浏览器', '电脑操作', 'LLM Wiki'])
      await expect(page.getByRole('article', { name: `插件 ${name}`, exact: true })).toHaveCount(1)
    await expect(page.locator('article.plugin-card')).toHaveCount(4)
    await expect(page.getByRole('article', { name: /^内置能力 / })).toHaveCount(0)
    await expect(page.getByRole('article', { name: '插件 Browser Use', exact: true })).toHaveCount(
      0,
    )
  }
  const pluginState = (id) =>
    page.evaluate(async (pluginId) => {
      const settings = await window.harness.extensionSettings()
      return settings.plugins.find((plugin) => plugin.id === pluginId) ?? null
    }, id)
  await expect(chrome).toBeVisible()
  await expectSingleCards()
  await expect(chrome.getByText('Chrome 未连接', { exact: true })).toBeVisible()
  await expect(chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true })).toBeDisabled()
  await expect(chrome.getByText(/Chrome 扩展尚未上架/)).toBeVisible()
  await expect(chrome.getByRole('button', { name: '安装', exact: true })).toHaveCount(0)
  const developer = chrome
    .locator('details')
    .filter({ has: page.locator('summary', { hasText: '开发者选项' }) })
  assert.equal(await developer.evaluate((element) => element.open), false)
  await developer.locator('summary').click()
  await expect(chrome.getByRole('button', { name: '打开本地扩展目录', exact: true })).toBeVisible()
  await developer.locator('summary').click()

  // UI setup owns installation now; the real bridge is still independently
  // verified here without invoking any browser connection or browsing tools.
  const settings = await page.evaluate(() => window.harness.installBundledPlugin('chrome-use'))
  const installed = settings.plugins.find((plugin) => plugin.id === 'chrome-use')
  assert(installed, 'the bundled Chrome plugin must be installed through the real bridge')
  assert.equal(installed.enabled, true)
  assert.equal(installed.skill_count, 1)
  const catalog = await page.evaluate(
    (id) => window.harness.testMcp(id),
    installed.mcp_servers[0].id,
  )
  assert.deepEqual(catalog.tools.map((tool) => tool.name).sort(), [
    'chrome_action',
    'chrome_open',
    'chrome_release',
    'chrome_screenshot',
    'chrome_select_tab',
    'chrome_snapshot',
    'chrome_status',
    'chrome_tabs',
  ])
  // A direct bridge installation does not invalidate the renderer query cache.
  // Reload to inspect the persisted card exactly as a returning user would.
  await page.reload()
  await expectSingleCards()
  await expect(chrome.getByRole('button', { name: '更新内置版本', exact: true })).toHaveCount(1)
  await expect(chrome.getByRole('switch', { name: '启用插件 Chrome', exact: true })).toBeChecked()
  await expect(chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true })).toBeDisabled()

  // Install and manage the browser plugin through the real UI. Installation
  // only saves plugin metadata; do not click Prepare browser or Test connection.
  await browser.getByRole('button', { name: '安装', exact: true }).click()
  await expect(browser.getByRole('button', { name: '更新内置版本', exact: true })).toBeVisible()
  await expectSingleCards()
  const browserToggle = browser.getByRole('switch', { name: '启用插件 独立浏览器', exact: true })
  await expect(browserToggle).toBeChecked()
  await browserToggle.click()
  await expect(browserToggle).not.toBeChecked()
  await expect.poll(async () => (await pluginState('browser-use'))?.enabled).toBe(false)
  await browserToggle.click()
  await expect(browserToggle).toBeChecked()
  await expect.poll(async () => (await pluginState('browser-use'))?.enabled).toBe(true)

  await browser.getByRole('button', { name: '卸载插件 独立浏览器', exact: true }).click()
  await browser.getByRole('button', { name: '确认卸载', exact: true }).click()
  await expect(browser.getByRole('button', { name: '安装', exact: true })).toBeVisible()
  await expect(browserToggle).toHaveCount(0)
  await expect.poll(() => pluginState('browser-use')).toBeNull()
  await expectSingleCards()
  await browser.getByRole('button', { name: '安装', exact: true }).click()
  await expect(browserToggle).toBeChecked()
  await expect(browser.getByRole('button', { name: '更新内置版本', exact: true })).toHaveCount(1)
  await expect.poll(async () => (await pluginState('browser-use'))?.enabled).toBe(true)

  // The computer card must also remain the same card after metadata installation;
  // this does not call the helper's OS permission-request operations.
  await computerCard.getByRole('button', { name: '安装', exact: true }).click()
  await expect(
    computerCard.getByRole('button', { name: '更新内置版本', exact: true }),
  ).toBeVisible()
  await expect(
    computerCard.getByRole('switch', { name: '启用插件 电脑操作', exact: true }),
  ).toBeChecked()
  await expectSingleCards()
  await expect(page.getByText('已安装 · 在下方管理', { exact: true })).toHaveCount(0)

  const plugins = await page.evaluate(() => window.harness.bundledPlugins())
  const realChrome = plugins.find((plugin) => plugin.id === 'chrome-use')
  assert.equal(
    realChrome.chrome.storeUrl,
    null,
    'this offline build must not invent a store listing',
  )
  assert.equal(realChrome.chrome.configured, false)
  assert.equal(realChrome.chrome.connected, false)
  const expectedExtensionPath = join(directory, 'chrome-use', 'extension')
  assert.equal(
    realChrome.chrome.extensionPath,
    expectedExtensionPath,
    'the folder picker path must be an ordinary user-data directory outside the application bundle',
  )
  await assert.rejects(access(expectedExtensionPath), { code: 'ENOENT' })
  await assert.rejects(readFile(join(directory, 'chrome-use', 'config.json')), { code: 'ENOENT' })
  const computer = plugins.find((plugin) => plugin.id === 'computer-use')
  assert.equal(computer.available, true)
  assert(computer.permissions.helperPath.endsWith('/Open Harness Computer Use.app'))
  assert.equal(typeof computer.permissions.accessibility, 'boolean', computer.permissions.error)
  assert.equal(typeof computer.permissions.screenRecording, 'boolean', computer.permissions.error)
  await expect(
    page.getByText(
      '为 Open Harness Computer Use 授予 macOS 系统权限后，再在会话中选择允许操作的应用。',
    ),
  ).toBeVisible()
  await page.screenshot({ path: join(artifacts, 'control-setup.png'), fullPage: true })

  // Deliberately fake a future published listing in this isolated main process.
  // No store URL is opened: the connect handler only records requested modes.
  await application.evaluate(({ ipcMain }, catalog) => {
    const fixture = {
      status: {
        ...catalog.find((plugin) => plugin.id === 'chrome-use').chrome,
        storeUrl: 'https://chromewebstore.google.com/detail/keibjmjjimfgkikahpjgbfebiglbjkee',
        configured: false,
        connected: false,
        allowNewTabs: false,
      },
      modes: [],
      statusCalls: 0,
      catalogCalls: 0,
      holdCatalog: false,
      catalogWaiters: [],
      catalogStatus: undefined,
      catalogDescription: undefined,
      copiedText: [],
    }
    globalThis.__controlSetupFixture = fixture
    for (const channel of [
      'openharness:chrome:status',
      'openharness:chrome:connect',
      'openharness:plugins:bundled',
      'openharness:clipboard:copy-text',
    ])
      ipcMain.removeHandler(channel)
    ipcMain.handle('openharness:chrome:status', () => {
      fixture.statusCalls += 1
      return { ...fixture.status }
    })
    ipcMain.handle('openharness:chrome:connect', (_event, mode) => {
      fixture.modes.push(mode)
      fixture.status.configured = true
    })
    ipcMain.handle('openharness:clipboard:copy-text', (_event, text) => {
      fixture.copiedText.push(text)
    })
    ipcMain.handle('openharness:plugins:bundled', async () => {
      fixture.catalogCalls += 1
      if (fixture.holdCatalog) await new Promise((resolve) => fixture.catalogWaiters.push(resolve))
      return catalog.map((plugin) =>
        plugin.id === 'chrome-use'
          ? {
              ...plugin,
              chrome: { ...(fixture.catalogStatus ?? fixture.status) },
              description: fixture.catalogDescription ?? plugin.description,
            }
          : plugin,
      )
    })
  }, plugins)
  await page.reload()
  await expectSingleCards()
  await expect(chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true })).toBeEnabled()
  await chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true }).click()
  await expect(chrome.getByText('等待 Chrome 连接…', { exact: true })).toBeVisible()
  assert.deepEqual(await application.evaluate(() => globalThis.__controlSetupFixture.modes), [
    'store',
  ])
  const statusCallsBeforeConnection = await application.evaluate(() => {
    const fixture = globalThis.__controlSetupFixture
    fixture.status.connected = true
    return fixture.statusCalls
  })
  await expect(chrome.getByText('Chrome 已连接', { exact: true })).toBeVisible({ timeout: 10_000 })
  assert(
    (await application.evaluate(() => globalThis.__controlSetupFixture.statusCalls)) >
      statusCallsBeforeConnection,
    'the connection must be detected by a subsequent status query without clicking Check connection',
  )
  await expect(chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true })).toHaveCount(0)
  await expect(chrome.getByText(/允许创建后台标签页，或仅共享当前网页/)).toBeVisible()
  await page.screenshot({
    path: join(artifacts, 'control-setup-connected-fixture.png'),
    fullPage: true,
  })

  // Model the slower bundled-catalog request returning a stale disconnected
  // snapshot after the dedicated connection query already confirmed success.
  // The gate fixes the response order without relying on an arbitrary delay.
  const staleDescription = '连接状态竞争测试：此插件目录快照已过期。'
  const beforeFocus = await application.evaluate((_electron, description) => {
    const fixture = globalThis.__controlSetupFixture
    fixture.catalogStatus = {
      ...fixture.status,
      configured: false,
      connected: false,
      extensionPath: '/stale-catalog-fixture/extension',
    }
    fixture.catalogDescription = description
    fixture.holdCatalog = true
    return { status: fixture.statusCalls, catalog: fixture.catalogCalls }
  }, staleDescription)
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await expect
    .poll(() => application.evaluate(() => globalThis.__controlSetupFixture.statusCalls))
    .toBeGreaterThan(beforeFocus.status)
  await expect
    .poll(() => application.evaluate(() => globalThis.__controlSetupFixture.catalogCalls))
    .toBeGreaterThan(beforeFocus.catalog)
  await expect(chrome.getByRole('button', { name: '检查连接', exact: true })).toBeEnabled()
  await application.evaluate(() => {
    const fixture = globalThis.__controlSetupFixture
    fixture.holdCatalog = false
    for (const release of fixture.catalogWaiters.splice(0)) release()
  })
  await expect(chrome.getByText(staleDescription, { exact: true })).toBeVisible()
  // Flush the render and passive effects before checking that the old catalog
  // could not overwrite the authoritative connection-query state.
  await page.evaluate(
    () =>
      new Promise((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
      }),
  )
  await expect(chrome.getByText('Chrome 已连接', { exact: true })).toBeVisible()
  await expect(chrome.getByRole('button', { name: '安装 Chrome 扩展', exact: true })).toHaveCount(0)
  await application.evaluate(() => {
    const fixture = globalThis.__controlSetupFixture
    fixture.catalogStatus = undefined
    fixture.catalogDescription = undefined
  })

  // Existing developer installations keep repair/development controls even if
  // distribution metadata is absent. These buttons remain mocked as above.
  await application.evaluate(() => {
    globalThis.__controlSetupFixture.status.storeUrl = null
  })
  await chrome.getByRole('button', { name: '检查连接', exact: true }).click()
  await developer.locator('summary').click()
  await chrome.getByRole('button', { name: '打开本地扩展目录', exact: true }).click()
  await expect(developer.getByText(/macOS 可按 ⌘⇧G/)).toBeVisible()
  await expect(developer.locator('code')).toHaveText(expectedExtensionPath)
  const copyDirectory = chrome.getByRole('button', { name: '复制扩展目录', exact: true })
  await expect(copyDirectory).toBeEnabled()
  await copyDirectory.click()
  await expect(chrome.getByRole('button', { name: '已复制目录', exact: true })).toBeVisible()
  assert.deepEqual(
    await application.evaluate(() => globalThis.__controlSetupFixture.copiedText),
    [expectedExtensionPath],
    'the copy button must send the exported directory path to the mocked clipboard IPC',
  )
  const help = chrome
    .locator('details')
    .filter({ has: page.locator('summary', { hasText: '连接帮助' }) })
  await help.locator('summary').click()
  await chrome.getByRole('button', { name: '启用并修复连接', exact: true }).click()
  await expect
    .poll(() => application.evaluate(() => globalThis.__controlSetupFixture.modes))
    .toEqual(['store', 'development', 'repair'])
  assert.deepEqual(errors, [])
  const report = {
    passed: true,
    executable: executable ?? 'development Electron with built output',
    chromePluginTools: catalog.tools.length,
    computerPermissions: computer.permissions,
    realChecks: [
      'unpublished store installation disabled with explicit explanation',
      'developer setup collapsed and no duplicate Chrome plugin install button',
      'Chrome plugin bridge installation and all 8 tool schemas',
      'one card per plugin before and after installing all three control plugins',
      'installed Chrome has one update action and its enable switch in the same card',
      'browser plugin UI installation, persisted toggle, uninstall and reinstallation without downloading Chromium',
      'canonical 独立浏览器 name without a separate Browser Use card',
      'disconnected status without registering a host or opening Chrome',
      'extension picker path is outside the app bundle and status checks do not export files',
      'independent Computer Use app permissions bridge',
      'no renderer errors',
    ],
    mockedUiChecks: [
      'published-store button invokes store mode',
      'waiting state automatically changes to connected after polling',
      'a delayed stale catalog cannot replace the dedicated live Chrome connection status',
      'connected extension still explains explicit background-tab permission',
      'development and repair modes remain available without a store URL',
      'copy extension directory sends the ordinary user-data path without modifying the real clipboard',
    ],
    limitation:
      'Store publication, extension export, Chrome installation and native connection are not simulated as real acceptance results.',
  }
  await writeFile(join(artifacts, 'control-setup-report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify(report, null, 2))
} finally {
  try {
    await application?.close()
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}
