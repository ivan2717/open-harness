import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-plugins-e2e-'))
const data = join(directory, 'data')
const source = join(directory, 'plugin-source')
const update = join(directory, 'plugin-update')
const invalid = join(directory, 'wrong-plugin')
const zip = join(directory, 'plugin-update.zip')
const methods = []
const server = createServer(async (request, response) => {
  if (request.method !== 'POST') return response.writeHead(405).end()
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  const message = JSON.parse(Buffer.concat(chunks).toString())
  methods.push(message.method)
  if (message.id === undefined) return response.writeHead(202).end()
  let result
  if (message.method === 'initialize')
    result = {
      protocolVersion: '2025-11-25',
      capabilities: { tools: {} },
      serverInfo: { name: 'plugin-fixture', version: '1' },
    }
  else if (message.method === 'tools/list')
    result = {
      tools: [{ name: 'review', description: 'Review fixture', inputSchema: { type: 'object' } }],
    }
  else throw new Error(`Unexpected MCP call: ${message.method}`)
  response.writeHead(200, { 'Content-Type': 'application/json' })
  response.end(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }))
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const mcpUrl = `http://127.0.0.1:${server.address().port}/mcp`
async function fixture(path, name, version) {
  await mkdir(join(path, 'skills/review/references'), { recursive: true })
  await writeFile(
    join(path, 'plugin.json'),
    JSON.stringify({ name, version, description: `Review workflow ${version}` }),
  )
  await writeFile(
    join(path, 'skills/review/SKILL.md'),
    `---\nname: review\ndescription: Plugin review workflow\n---\nPlugin guidance ${version}. Read references/criteria.md.\n`,
  )
  await writeFile(join(path, 'skills/review/references/criteria.md'), `Criteria ${version}`)
  await writeFile(
    join(path, 'mcp.json'),
    JSON.stringify({ mcpServers: { fixture: { type: 'http', url: mcpUrl } } }),
  )
}
await fixture(source, 'org.review', '1.0.0')
await fixture(update, 'org.review', '2.0.0')
await fixture(invalid, 'wrong-plugin', '3.0.0')
await mkdir(join(data, 'runtime/skills/review'), { recursive: true })
await writeFile(
  join(data, 'runtime/skills/review/SKILL.md'),
  '---\nname: review\ndescription: Personal workflow\n---\nPersonal guidance.\n',
)
await promisify(execFile)('python3', [
  '-c',
  `import pathlib,sys,zipfile
root=pathlib.Path(sys.argv[1])
with zipfile.ZipFile(sys.argv[2], 'w', zipfile.ZIP_DEFLATED) as archive:
 for path in root.rglob('*'):
  if path.is_file(): archive.write(path, path.relative_to(root))`,
  update,
  zip,
])

const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: data }
delete env.ELECTRON_RUN_AS_NODE
let application
const errors = []
async function launch() {
  application = await _electron.launch({
    executablePath: require('electron'),
    args: [resolve('.')],
    env,
    timeout: 30_000,
  })
  const page = await application.firstWindow()
  page.on('pageerror', (error) => errors.push(error.message))
  await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
  await page.evaluate(() => {
    window.location.hash = '/extensions'
  })
  await expect(page.getByRole('heading', { name: '插件', exact: true })).toBeVisible()
  return page
}
async function select(path) {
  await application.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({
      canceled: path === null,
      filePaths: path === null ? [] : [path],
    })
  }, path)
}
async function settings(page) {
  return page.evaluate(() => window.harness.extensionSettings())
}
try {
  let page = await launch()
  await page.evaluate(async () => {
    const { config } = await window.harness.extensionSettings()
    await window.harness.saveExtensions({
      ...config,
      mcp_servers: [
        {
          id: 'manual',
          enabled: false,
          transport: { type: 'http', url: 'https://example.invalid/mcp' },
        },
      ],
    })
  })
  await select(source)
  await page.getByRole('button', { name: '安装插件', exact: true }).click()
  await page.getByRole('menuitem', { name: '从文件夹安装', exact: true }).click()
  let card = page.getByRole('article', { name: '插件 org.review', exact: true })
  await expect(card).toBeVisible()
  await expect(card.getByText('1.0.0 · 已启用', { exact: true })).toBeVisible()
  assert.deepEqual(methods, [], 'install does not connect to MCP')
  let snapshot = await settings(page)
  assert.deepEqual(snapshot.skills.map((skill) => skill.id).sort(), ['org.review:review', 'review'])
  assert.equal(snapshot.plugins[0].skill_count, 1)
  assert.equal(snapshot.plugins[0].mcp_servers.length, 1)
  assert.equal(snapshot.config.mcp_servers[0].id, 'manual')
  const firstHash = snapshot.plugins[0].content_hash
  const beforeSelection = snapshot.config
  await page.getByRole('link', { name: '会话', exact: true }).click()
  await page.getByRole('button', { name: '添加内容', exact: true }).click()
  await page.getByRole('menuitem', { name: '使用技能', exact: true }).click()
  const skills = page.getByRole('dialog', { name: '选择技能', exact: true })
  await expect(skills.getByRole('button', { name: '使用技能 review', exact: true })).toBeVisible()
  await expect(
    skills.getByRole('button', { name: '使用技能 org.review:review', exact: true }),
  ).toBeVisible()
  await skills.getByRole('textbox', { name: '搜索技能', exact: true }).fill('org.review')
  await expect(skills.getByRole('button', { name: '使用技能 review', exact: true })).toHaveCount(0)
  await skills.getByRole('button', { name: '使用技能 org.review:review', exact: true }).click()
  await expect(page.getByRole('textbox', { name: '任务描述', exact: true })).toHaveValue(
    '$org.review:review ',
  )
  assert.deepEqual((await settings(page)).config, beforeSelection)
  assert.deepEqual(methods, [], 'selecting a plugin skill must not contact its MCP service')
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 0)
  await page.getByRole('link', { name: '插件', exact: true }).click()
  await card.locator('.plugin-details > summary').click()
  await expect(card.getByRole('switch', { name: '启用技能 org.review:review' })).toBeVisible()
  await card.getByRole('button', { name: '测试连接', exact: true }).click()
  await expect(card.getByText(/已连接 · 1 个工具/)).toBeVisible({ timeout: 20_000 })
  assert(methods.includes('initialize') && methods.includes('tools/list'))
  assert(!methods.includes('tools/call'))
  const toggle = card.getByRole('switch', { name: '启用插件 org.review', exact: true })
  await toggle.click()
  await expect(toggle).not.toBeChecked()
  assert.deepEqual(
    (await settings(page)).skills.map((skill) => skill.id),
    ['review'],
  )
  await toggle.click()
  await expect(toggle).toBeChecked()
  const skillToggle = card.getByRole('switch', { name: '启用技能 org.review:review', exact: true })
  await skillToggle.click()
  await expect(skillToggle).not.toBeChecked()
  assert.deepEqual((await settings(page)).config.disabled_skill_ids, ['org.review:review'])
  await select(null)
  await card.getByRole('button', { name: '更新插件 org.review' }).click()
  await page.getByRole('menuitem', { name: '从 ZIP 更新' }).click()
  await expect(card.getByText('1.0.0 · 已启用', { exact: true })).toBeVisible()
  await select(zip)
  await card.getByRole('button', { name: '更新插件 org.review' }).click()
  await page.getByRole('menuitem', { name: '从 ZIP 更新' }).click()
  await expect(card.getByText('2.0.0 · 已启用', { exact: true })).toBeVisible()
  snapshot = await settings(page)
  assert.notEqual(snapshot.plugins[0].content_hash, firstHash)
  assert.equal(snapshot.skills.find((skill) => skill.id === 'org.review:review').enabled, false)
  assert.equal(snapshot.skills.find((skill) => skill.id === 'review').enabled, true)
  await mkdir(resolve('test-results'), { recursive: true })
  await page.screenshot({
    path: resolve('test-results/desktop-plugins-settings.png'),
    fullPage: true,
  })
  await select(invalid)
  await card.getByRole('button', { name: '更新插件 org.review' }).click()
  await page.getByRole('menuitem', { name: '从文件夹更新' }).click()
  await expect(
    page.getByText('请选择同一插件的新版本，当前插件保持不变。', { exact: true }),
  ).toBeVisible()
  assert.equal((await settings(page)).plugins[0].version, '2.0.0')
  await mkdir(resolve('test-results'), { recursive: true })
  await page.screenshot({
    path: resolve('test-results/desktop-plugins-update-error.png'),
    fullPage: true,
  })
  await application.close()
  application = undefined
  page = await launch()
  snapshot = await settings(page)
  assert.equal(snapshot.plugins[0].version, '2.0.0')
  assert.equal(snapshot.skills.find((skill) => skill.id === 'org.review:review').enabled, false)
  card = page.getByRole('article', { name: '插件 org.review', exact: true })
  await card.getByRole('button', { name: '卸载插件 org.review' }).click()
  await card.getByRole('button', { name: '确认卸载', exact: true }).click()
  await expect(card).toHaveCount(0)
  snapshot = await settings(page)
  assert.deepEqual(snapshot.plugins, [])
  assert.deepEqual(
    snapshot.skills.map((skill) => skill.id),
    ['review'],
  )
  assert.equal(snapshot.config.mcp_servers[0].id, 'manual')
  assert.equal(JSON.parse(await readFile(join(source, 'plugin.json'), 'utf8')).version, '1.0.0')
  assert.deepEqual(errors, [])
  console.log(
    JSON.stringify(
      {
        passed: true,
        directory,
        checks: [
          'native directory install',
          'lazy MCP connection',
          'namespaced skills',
          'namespaced skill selection without execution or configuration changes',
          'plugin enable/disable',
          'ZIP update',
          'cancel and failed update preserve state',
          'restart persistence',
          'uninstall preserves standalone skills and MCP',
        ],
      },
      null,
      2,
    ),
  )
} finally {
  await application?.close()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
