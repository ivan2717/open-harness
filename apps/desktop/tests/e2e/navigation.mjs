import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, realpath, rm, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { basename, join, resolve } from 'node:path'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-navigation-e2e-'))
const dataDirectory = join(directory, 'app-data')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const projects = await Promise.all(
  ['alpha/Sample project', 'beta/Sample project', 'Empty project'].map(async (name) => {
    const path = join(directory, name)
    await mkdir(path, { recursive: true })
    return realpath(path)
  }),
)
const [alpha, beta, empty] = projects
const reference = join(directory, 'alpha-notes.md')
await writeFile(reference, '# Notes belonging only to the alpha project draft.\n')
const server = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.writeHead(404)
    response.end()
    return
  }
  for await (const _chunk of request) {
    /* consume the bounded test request */
  }
  response.writeHead(200, { 'Content-Type': 'text/event-stream' })
  response.write(
    `data: ${JSON.stringify({
      id: 'navigation-fixture-response',
      choices: [
        {
          index: 0,
          delta: { role: 'assistant', content: '项目会话已就绪。' },
          finish_reason: 'stop',
        },
      ],
    })}\n\n`,
  )
  response.write(
    `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 1000, completion_tokens: 20 } })}\n\n`,
  )
  response.end('data: [DONE]\n\n')
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  model: 'navigation-fixture',
  api_key: 'navigation-local-fixture-key',
  context_window: 64000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
let fixtureModel
const errors = []
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: dataDirectory }
delete env.ELECTRON_RUN_AS_NODE
let application
let page
let rail
let sidebar
let composer
const link = (name) => rail.getByRole('link', { name, exact: true })
const projectGroup = (path) => sidebar.locator(`[data-project-path=${JSON.stringify(path)}]`)
const projectNew = (path) =>
  projectGroup(path).getByRole('button', { name: `在 ${basename(path)} 中新建会话`, exact: true })
const draftFiles = () => page.locator('.composer .file-references li')
const generalDraft = 'General task draft stays separate from every project.'
const alphaDraft = 'Alpha project draft with its own file reference.'
const betaDraft = 'Beta project draft, independent despite the identical folder name.'

async function launch() {
  application = await _electron.launch({
    executablePath: process.env.OPENHARNESS_E2E_EXECUTABLE ?? require('electron'),
    args: process.env.OPENHARNESS_E2E_EXECUTABLE ? [] : [resolve('.')],
    env,
    timeout: 30_000,
  })
  page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await application.evaluate(({ safeStorage }) => {
    safeStorage.isEncryptionAvailable = () => false
  })
  rail = page.getByRole('navigation', { name: '主导航' })
  sidebar = page.getByRole('complementary', { name: '会话导航' })
  composer = page.getByRole('textbox', { name: '任务描述', exact: true })
}

async function expectSection(name, path, heading) {
  await expect.poll(() => page.evaluate(() => window.location.hash || '#/')).toContain(path)
  await expect(link(name)).toHaveAttribute('aria-current', 'page')
  await expect(rail.locator('[aria-current="page"]')).toHaveCount(1)
  if (heading) await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible()
  for (const label of ['会话', '插件', '模型', '设置']) await expect(link(label)).toBeInViewport()
  await expect(link('项目')).toHaveCount(0)
}

async function picker(paths = [], error = null) {
  await application.evaluate(
    ({ dialog }, { paths, error }) => {
      globalThis.__navigationPicker = { calls: 0, options: null }
      dialog.showOpenDialog = async (_window, options) => {
        globalThis.__navigationPicker.calls += 1
        globalThis.__navigationPicker.options = options
        if (error) throw new Error(error)
        return { canceled: !paths.length, filePaths: paths }
      }
    },
    { paths, error },
  )
}

async function addProject(path) {
  await picker([path])
  await sidebar.getByRole('button', { name: '添加项目', exact: true }).click()
  await expectSection('会话', '#/', '开始一个新任务')
  await expect(page.getByRole('button', { name: '项目', exact: true })).toHaveAttribute(
    'title',
    path,
  )
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '请求批准',
  )
  await expect(projectGroup(path)).toBeVisible()
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 1)
}

async function expectProjectDraft(path, draft) {
  await expectSection('会话', '#/', '开始一个新任务')
  await expect(composer).toHaveValue(draft)
  await expect(composer).toBeFocused()
  await expect(page.getByRole('button', { name: '项目', exact: true })).toHaveAttribute(
    'title',
    path,
  )
  await expect(page.getByRole('button', { name: '项目权限', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '审批方式', exact: true })).toContainText(
    '请求批准',
  )
}

async function openRun(id) {
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, id), {
      timeout: 45_000,
    })
    .toBe('waiting_for_input')
  await expect(sidebar.locator(`.run-link[href$="/runs/${id}"]`)).toBeVisible()
}

async function createFixture(prompt, path) {
  if (!fixtureModel) {
    await link('模型').click()
    await page.getByLabel('接口协议').selectOption(configuration.protocol)
    await page.getByLabel('API 基础地址').fill(configuration.base_url)
    await page.getByLabel('模型名称', { exact: true }).fill(configuration.model)
    await page.getByLabel('API key', { exact: true }).fill(configuration.api_key)
    await page.getByRole('button', { name: '保存并使用', exact: true }).click()
    await expect(page.getByRole('status')).toContainText('模型已接入')
    fixtureModel = (await page.evaluate(() => window.harness.config())).models.find(
      (model) => model.name === configuration.model,
    ).id
    await link('会话').click()
  }
  const run = await page.evaluate(
    ({ prompt, path, model }) =>
      window.harness.createRun({
        prompt: `/ask ${prompt}`,
        model_id: model,
        ...(path ? { workspace_path: path, permission_mode: 'read_only' } : {}),
      }),
    { prompt, path, model: fixtureModel },
  )
  await openRun(run.id)
  return run
}

try {
  await launch()
  await expectSection('会话', '#/', '开始一个新任务')
  await expect(sidebar).toBeVisible()
  await composer.fill(generalDraft)
  await page.screenshot({ path: join(artifacts, 'desktop-navigation-home.png') })

  // The rail remains usable while the separate conversation panel is collapsed.
  await page.getByRole('button', { name: '收起侧边栏', exact: true }).click()
  await expect(sidebar).toBeHidden()
  await link('模型').click()
  await expectSection('模型', '/models', '接入模型')
  await expect(sidebar).toBeHidden()
  await expect(page.getByRole('heading', { name: '界面偏好' })).toHaveCount(0)
  await link('会话').click()
  await expect(composer).toHaveValue(generalDraft)
  await expect(sidebar).toBeHidden()
  await page.getByRole('button', { name: '展开侧边栏', exact: true }).click()
  await expect(sidebar).toBeVisible()

  await link('插件').hover()
  await expect(page.getByRole('tooltip').filter({ hasText: '插件与技能' })).toBeVisible()
  await page.mouse.move(600, 400)
  await link('会话').focus()
  await page.keyboard.press('Tab')
  await expect(link('插件')).toBeFocused()
  await expect(page.getByRole('tooltip').filter({ hasText: '插件与技能' })).toBeVisible()
  await page.keyboard.press('Enter')
  await expectSection('插件', '/extensions', '扩展')
  await expect(sidebar).toBeHidden()
  const sections = page.getByRole('navigation', { name: '扩展分类' })
  await expect(sections.getByRole('link', { name: '插件', exact: true })).toHaveAttribute(
    'aria-current',
    'page',
  )
  await expect(page.getByRole('button', { name: '安装插件', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: '工具服务', exact: true })).toHaveCount(0)
  await page.screenshot({ path: join(artifacts, 'desktop-navigation-extensions.png') })
  for (const [tab, label, heading] of [
    ['skills', '技能', '技能'],
    ['mcp', 'MCP 服务', '工具服务'],
  ]) {
    await sections.getByRole('link', { name: label, exact: true }).click()
    await expectSection('插件', `tab=${tab}`, heading)
    await expect(sections.getByRole('link', { name: label, exact: true })).toHaveAttribute(
      'aria-current',
      'page',
    )
    await expect(page.getByRole('button', { name: '安装插件', exact: true })).toHaveCount(0)
  }
  await page.reload()
  await expectSection('插件', 'tab=mcp', '工具服务')
  await link('设置').click()
  await expectSection('设置', '/settings', '界面偏好')
  await expect(page.getByRole('heading', { name: '接入模型', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '安装插件', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '运行上下文演示', exact: true })).toBeHidden()
  await page.evaluate(() => {
    window.location.hash = '/projects'
  })
  await expectSection('会话', '#/', '开始一个新任务')
  await expect.poll(() => page.evaluate(() => window.location.hash || '#/')).toBe('#/')

  // Projects are conversation groups. Adding one is explicit native selection;
  // cancelling or rejecting the picker leaves the current draft intact.
  await picker()
  await sidebar.getByRole('button', { name: '添加项目', exact: true }).click()
  await expect(composer).toHaveValue(generalDraft)
  await expect(sidebar.locator('.sidebar-project')).toHaveCount(0)
  await picker([], 'NAVIGATION_PICKER_FAILURE')
  await sidebar.getByRole('button', { name: '添加项目', exact: true }).click()
  await expect(sidebar).toContainText('NAVIGATION_PICKER_FAILURE')
  await expect(composer).toHaveValue(generalDraft)
  await addProject(alpha)
  await expect(composer).toHaveValue('')
  await composer.fill(alphaDraft)
  await picker([reference])
  await page.getByRole('button', { name: '添加内容', exact: true }).click()
  await page.getByRole('menuitem', { name: '添加文件', exact: true }).click()
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles()).toContainText('alpha-notes.md')
  await addProject(beta)
  await expect(composer).toHaveValue('')
  await expect(draftFiles()).toHaveCount(0)
  await composer.fill(betaDraft)
  await addProject(empty)
  await expect(composer).toHaveValue('')
  await expect(sidebar.locator('.sidebar-project')).toHaveCount(3)
  await picker([], 'An already granted project should not reopen the picker.')
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  await expect(draftFiles()).toHaveCount(1)
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  await projectNew(beta).click()
  await expectProjectDraft(beta, betaDraft)
  await expect(draftFiles()).toHaveCount(0)
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 0)

  // Changing the folder remains an explicit picker action, separate from new chat.
  await picker([alpha])
  await page.getByRole('button', { name: '项目', exact: true }).click()
  await page.getByRole('menuitem', { name: '更换项目', exact: true }).click()
  await expect(composer).toHaveValue(alphaDraft)
  await expect(page.getByRole('button', { name: '项目', exact: true })).toHaveAttribute(
    'title',
    alpha,
  )
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 1)
  await sidebar.getByRole('link', { name: /新建会话/ }).click()
  await expect(composer).toHaveValue(generalDraft)
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  await expect(draftFiles()).toHaveCount(0)

  // A local fixture provider exercises actual persisted project conversations.
  const generalRun = await createFixture('General navigation fixture')
  const alphaRun = await createFixture('Alpha unique session', alpha)
  const betaRun = await createFixture('Beta unique session', beta)
  await expect(
    sidebar.getByRole('navigation', { name: '会话', exact: true }).locator('.run-link'),
  ).toHaveCount(1)
  await expect(projectGroup(alpha).locator('.run-link')).toHaveCount(1)
  await expect(projectGroup(beta).locator('.run-link')).toHaveCount(1)
  await expect(projectGroup(empty).locator('.run-link')).toHaveCount(0)
  for (const run of [generalRun, alphaRun, betaRun]) {
    await expect(sidebar.locator(`.run-link[href$="/runs/${run.id}"]`)).toHaveCount(1)
  }
  await projectGroup(alpha)
    .getByRole('button', { name: `收起项目 ${basename(alpha)}`, exact: true })
    .click()
  await expect(projectGroup(alpha).locator('.run-link')).toBeHidden()
  await openRun(alphaRun.id)
  await expect(
    projectGroup(alpha).getByRole('button', { name: `收起项目 ${basename(alpha)}`, exact: true }),
  ).toHaveAttribute('aria-expanded', 'true')
  await projectGroup(beta)
    .getByRole('button', { name: `收起项目 ${basename(beta)}`, exact: true })
    .click()
  await sidebar.getByRole('textbox', { name: '搜索会话' }).fill('Beta unique')
  await expect(projectGroup(beta).locator('.run-link')).toBeVisible()
  await expect(sidebar.locator('.run-link')).toHaveCount(1)
  await sidebar.getByRole('textbox', { name: '搜索会话' }).fill('Sample project')
  await expect(sidebar.locator('.sidebar-project')).toHaveCount(2)
  await expect(sidebar.locator('.run-link')).toHaveCount(2)
  await sidebar.getByRole('textbox', { name: '搜索会话' }).fill('nothing-matches-this')
  await expect(sidebar.locator('.run-link')).toHaveCount(0)
  await sidebar.getByRole('textbox', { name: '搜索会话' }).fill('')
  await page.mouse.move(600, 400)
  await page.screenshot({ path: join(artifacts, 'desktop-navigation-projects.png') })

  await composer.fill('Follow-up draft belongs to the alpha conversation.')
  for (const section of ['插件', '模型', '设置']) {
    await link(section).click()
    await expect(sidebar).toBeHidden()
    await link('会话').click()
    await expectSection('会话', `/runs/${alphaRun.id}`)
    await expect(composer).toHaveValue('Follow-up draft belongs to the alpha conversation.')
  }
  await picker()
  await sidebar.getByRole('button', { name: '添加项目', exact: true }).click()
  await expectSection('会话', `/runs/${alphaRun.id}`)
  await expect(composer).toHaveValue('Follow-up draft belongs to the alpha conversation.')
  await picker([], 'New chat must not open a directory picker.')
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  await expect(draftFiles()).toHaveCount(1)
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 3)
  await projectGroup(alpha)
    .getByRole('button', { name: `收起项目 ${basename(alpha)}`, exact: true })
    .click()
  await expect(projectGroup(alpha).locator('.run-link')).toBeHidden()
  await projectNew(alpha).click()
  await expect(projectGroup(alpha).locator('.run-link')).toBeVisible()
  await expectProjectDraft(alpha, alphaDraft)
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+n' : 'Control+n')
  await expect(composer).toHaveValue(generalDraft)
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()

  await application.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(1000, 680),
  )
  await expect.poll(() => page.evaluate(() => window.innerWidth)).toBe(1000)
  await expectSection('会话', '#/', '开始一个新任务')
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeInViewport()
  await expect(sidebar.getByRole('button', { name: '添加项目', exact: true })).toBeInViewport()
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth),
    false,
  )
  await page.screenshot({ path: join(artifacts, 'desktop-navigation-compact.png') })
  await link('插件').click()
  await expect(page.getByRole('button', { name: '安装插件', exact: true })).toBeInViewport()
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth),
    false,
  )

  // Restart preserves main-owned known projects and text drafts. Opening an
  // existing project does not ask for its directory again or create an empty run.
  await application.close()
  application = undefined
  await launch()
  fixtureModel = (
    await page.evaluate((input) => window.harness.configureModel(input), configuration)
  ).model.id
  await page.reload()
  await expect(sidebar.locator('.sidebar-project')).toHaveCount(3)
  await expect(composer).toHaveValue(generalDraft)
  await assert.rejects(
    page.evaluate(
      (path) =>
        window.harness.createRun({
          prompt: 'Project not opened in this session',
          model_id: 'local-demo',
          workspace_path: path,
        }),
      alpha,
    ),
    /选择项目|打开项目/,
  )
  await picker([], 'A remembered project should reopen without a native picker.')
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  await expect(draftFiles()).toHaveCount(0)
  await projectNew(empty).click()
  await expectProjectDraft(empty, '')
  await expect(projectGroup(empty).locator('.run-link')).toHaveCount(0)
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 3)
  const reopenedRun = await createFixture('Authorized after restart', alpha)
  await expect(projectGroup(alpha).locator('.run-link')).toHaveCount(2)

  // Older installations have persisted conversations but no main-owned registry.
  // Their trusted runtime history still allows opening the original project.
  await application.close()
  application = undefined
  await rm(join(dataDirectory, 'workspaces.json'))
  await launch()
  await expect(sidebar.locator('.run-link')).toHaveCount(4)
  await picker([], 'A legacy history project should not open the picker.')

  // A delayed legacy lookup must not pull the user back after they leave for
  // another page. Gate only the next runtime-history fetch in the main process.
  await application.evaluate(() => {
    const fetch = globalThis.fetch
    globalThis.__navigationHistoryPending = false
    globalThis.fetch = async (...args) => {
      if (new URL(String(args[0])).pathname === '/api/runs') {
        globalThis.fetch = fetch
        globalThis.__navigationHistoryPending = true
        await new Promise((resolve) => {
          globalThis.__navigationHistoryRelease = resolve
        })
      }
      return fetch(...args)
    }
  })
  await projectNew(alpha).click()
  await expect
    .poll(() => application.evaluate(() => globalThis.__navigationHistoryPending))
    .toBe(true)
  await link('模型').click()
  await expectSection('模型', '/models', '接入模型')
  await application.evaluate(() => globalThis.__navigationHistoryRelease())
  await expect(
    page.locator(`[data-project-path=${JSON.stringify(alpha)}] .sidebar-project-create`),
  ).toBeEnabled()
  await expectSection('模型', '/models', '接入模型')
  await link('会话').click()
  await expect(composer).toHaveValue(generalDraft)
  await expect(page.getByRole('button', { name: '选择项目', exact: true })).toBeVisible()
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 4)

  // Sidebar metadata is not a filesystem grant. Forging a remembered entry must
  // fail in place rather than opening a picker or changing the current project.
  const unknown = join(directory, 'Unknown project')
  await mkdir(unknown)
  await page.evaluate((path) => {
    const saved = JSON.parse(localStorage.getItem('open-harness-workspace'))
    saved.state.projectPaths.push(path)
    localStorage.setItem('open-harness-workspace', JSON.stringify(saved))
  }, unknown)
  await page.reload()
  await expect(projectGroup(unknown)).toBeVisible()
  await projectNew(alpha).click()
  await expectProjectDraft(alpha, alphaDraft)
  await projectNew(unknown).click()
  await expect(sidebar.getByRole('alert')).toBeVisible()
  await expect(composer).toHaveValue(alphaDraft)
  await expect(page.getByRole('button', { name: '项目', exact: true })).toHaveAttribute(
    'title',
    alpha,
  )
  await assert.rejects(page.evaluate((path) => window.harness.openWorkspace(path), unknown))
  assert.equal(await application.evaluate(() => globalThis.__navigationPicker.calls), 0)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 4)
  assert.deepEqual(errors, [])
  console.log(
    JSON.stringify(
      {
        dataDirectory,
        projects,
        runs: [generalRun.id, alphaRun.id, betaRun.id, reopenedRun.id],
        checks:
          'rail, project hierarchy, isolated drafts/files, search, direct project reopening, restart, legacy migration, delayed navigation, forged metadata rejection, compact layout',
      },
      null,
      2,
    ),
  )
} finally {
  if (application) await application.close()
  await new Promise((resolve) => server.close(resolve))
}
