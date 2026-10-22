import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, writeFile, copyFile, readFile, realpath } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-extensions-e2e-'))
const data = join(directory, 'data')
const project = join(directory, 'project')
await mkdir(project)
const root = await realpath(project)
const skill = join(project, '.agents/skills/review')
await mkdir(join(skill, 'references'), { recursive: true })
await writeFile(
  join(skill, 'SKILL.md'),
  '---\nname: review\ndescription: Evaluate candidates using the handbook\n---\nRead references/criteria.md before comparing documents. SKILL_BODY_MARKER\n',
)
await writeFile(
  join(skill, 'references/criteria.md'),
  'Compare factual evidence; never invent experience. RESOURCE_BODY_MARKER\n',
)
for (const file of ['handbook.docx', 'resume.pdf'])
  await copyFile(resolve('../../crates/agent-local/tests/fixtures', file), join(project, file))
const globalSkill = join(data, 'runtime/skills/global-review')
await mkdir(globalSkill, { recursive: true })
await writeFile(
  join(globalSkill, 'SKILL.md'),
  '---\nname: global-review\ndescription: Global review workflow\n---\nGlobal fixture guidance.\n',
)
const mcpFile = join(directory, 'mcp.mjs')
const callsFile = join(directory, 'mcp-calls.jsonl')
await writeFile(
  mcpFile,
  `
import { createInterface } from 'node:readline';
import { appendFileSync } from 'node:fs';
let calls=0;
for await (const line of createInterface({input:process.stdin})) {
 const req=JSON.parse(line); if(req.id===undefined)continue;
 const p=req.params??{}; let result;
 switch(req.method) {
 case 'initialize': result={protocolVersion:'2025-11-25',capabilities:{tools:{},resources:{},prompts:{}},serverInfo:{name:'stateful-fixture',version:'1'}}; break;
 case 'tools/list': result={tools:[{name:'counter',description:'Increment a session counter',inputSchema:{type:'object'}}]}; break;
 case 'resources/list': result={resources:[{uri:'fixture:///manual',name:'Manual'}]}; break;
 case 'resources/templates/list': result={resourceTemplates:[]}; break;
 case 'prompts/list': result={prompts:[{name:'review',description:'Review a candidate'}]}; break;
 case 'tools/call': calls++; appendFileSync(process.argv[2],JSON.stringify({method:req.method,name:p.name,calls})+'\\n'); result={content:[{type:'text',text:'MCP_TOOL_OK'}],structuredContent:{calls},isError:false}; break;
 case 'resources/read': appendFileSync(process.argv[2],JSON.stringify({method:req.method})+'\\n'); result={contents:[{uri:p.uri,mimeType:'text/plain',text:'MCP_RESOURCE_OK'}]}; break;
 case 'prompts/get': appendFileSync(process.argv[2],JSON.stringify({method:req.method})+'\\n'); result={messages:[{role:'user',content:{type:'text',text:'MCP_PROMPT_OK'}}]}; break;
 default: throw Error(req.method);
 }
 process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:req.id,result})+'\\n');
}
`,
)
const plans = {
  DOCUMENTS: [
    ['list_skills', {}],
    ['load_skill', { id: 'review' }],
    ['read_skill_resource', { id: 'review', path: 'references/criteria.md' }],
    ['read_document', { path: 'handbook.docx' }],
    ['search_document', { path: 'resume.pdf', query: 'Rust' }],
  ],
  MCP: [
    ['list_mcp_servers', {}],
    ['list_mcp_tools', { server_id: 'fixture' }],
    ['call_mcp_tool', { server_id: 'fixture', name: 'counter', arguments: {} }],
    ['call_mcp_tool', { server_id: 'fixture', name: 'counter', arguments: {} }],
  ],
  RESOURCES: [
    ['list_mcp_resources', { server_id: 'fixture' }],
    ['read_mcp_resource', { server_id: 'fixture', uri: 'fixture:///manual' }],
    ['list_mcp_prompts', { server_id: 'fixture' }],
    ['get_mcp_prompt', { server_id: 'fixture', name: 'review', arguments: {} }],
  ],
  DENY: [['call_mcp_tool', { server_id: 'fixture', name: 'counter', arguments: { denied: true } }]],
  CANCEL: [
    ['call_mcp_tool', { server_id: 'fixture', name: 'counter', arguments: { cancelled: true } }],
  ],
  DISABLED: [
    ['load_skill', { id: 'review' }],
    ['list_mcp_servers', {}],
  ],
}
const counts = new Map()
const modelRequests = []
let sequence = 0
const server = createServer(async (request, response) => {
  if (request.method !== 'POST') {
    response.writeHead(404).end()
    return
  }
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  const body = JSON.parse(Buffer.concat(chunks).toString())
  modelRequests.push(body)
  const prompt = body.messages.filter((message) => message.role === 'user').at(-1).content
  const count = counts.get(prompt) ?? 0
  counts.set(prompt, count + 1)
  const call = plans[prompt]?.[count]
  response.writeHead(200, { 'Content-Type': 'text/event-stream' })
  response.write(
    `data: ${JSON.stringify({ id: `response-${++sequence}`, choices: [{ index: 0, delta: call ? { role: 'assistant', tool_calls: [{ index: 0, id: `tool-${sequence}`, type: 'function', function: { name: call[0], arguments: JSON.stringify(call[1]) } }] } : { role: 'assistant', content: '验证完成。' }, finish_reason: call ? 'tool_calls' : 'stop' }] })}\n\n`,
  )
  response.end('data: [DONE]\n\n')
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const modelInput = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  api_key: 'fixture',
  model: 'extension-fixture',
  context_window: 128000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
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
  return page
}
async function settled(page, id) {
  await expect
    .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), id)).status, {
      timeout: 30_000,
    })
    .toBe('waiting_for_input')
  return page.evaluate((id) => window.harness.getRun(id), id)
}
async function approval(page, id, allow) {
  await expect
    .poll(
      async () =>
        (await page.evaluate((id) => window.harness.runExtensions(id), id)).approvals.length,
    )
    .toBe(1)
  const pending = (await page.evaluate((id) => window.harness.runExtensions(id), id)).approvals[0]
  await page.evaluate(
    ({ id, approvalId, allow }) => window.harness.approveExtension(id, approvalId, allow),
    { id, approvalId: pending.id, allow },
  )
  return pending
}
try {
  let page = await launch()
  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    modelInput,
  )
  await application.evaluate(({ dialog }, root) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [root] })
  }, root)
  await page.evaluate(() => window.harness.chooseWorkspace())
  const extensionConfig = {
    mcp_servers: [
      {
        id: 'fixture',
        name: 'Fixture MCP',
        enabled: true,
        transport: {
          type: 'stdio',
          command: process.execPath,
          args: [mcpFile, callsFile],
          env_vars: [],
        },
      },
    ],
    disabled_skill_ids: [],
  }
  await page.evaluate(() => {
    window.location.hash = '/settings'
  })
  await expect(page.getByRole('heading', { name: '工具服务', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '添加服务', exact: true }).click()
  await page.getByLabel('标识', { exact: true }).fill('fixture')
  await page.getByLabel('显示名称', { exact: true }).fill('Fixture MCP')
  await page.getByLabel('启动命令', { exact: true }).fill(process.execPath)
  await page.getByLabel('参数 · 每行一个', { exact: true }).fill([mcpFile, callsFile].join('\n'))
  await page.getByRole('button', { name: '保存服务', exact: true }).click()
  await expect(page.getByRole('button', { name: '保存服务', exact: true })).toHaveCount(0)
  const saved = await page.evaluate(() => window.harness.extensionSettings())
  assert.deepEqual(saved.config.mcp_servers, extensionConfig.mcp_servers)
  assert.deepEqual(saved.config.disabled_skill_ids, [])
  assert.equal(typeof saved.config.revision, 'number')
  await page.getByRole('button', { name: '测试连接', exact: true }).click()
  await expect(page.getByText('已连接 · 1 个工具', { exact: true })).toBeVisible({
    timeout: 20_000,
  })
  assert.equal(
    await readFile(callsFile, 'utf8').catch(() => ''),
    '',
    'connection test must not execute tools',
  )
  await expect(page.getByText('global-review', { exact: true })).toBeVisible()
  const globalSwitch = page.getByRole('switch', { name: '启用技能 global-review', exact: true })
  await globalSwitch.click()
  await expect(globalSwitch).not.toBeChecked()
  assert.deepEqual(
    (await page.evaluate(() => window.harness.extensionSettings())).config.disabled_skill_ids,
    ['global-review'],
  )
  await globalSwitch.click()
  await expect(globalSwitch).toBeChecked()
  await page.getByRole('button', { name: '编辑 Fixture MCP', exact: true }).click()
  await expect(page.getByRole('textbox', { name: '参数 · 每行一个', exact: true })).toHaveValue(
    [mcpFile, callsFile].join('\n'),
  )
  await page.getByRole('button', { name: '取消', exact: true }).click()
  // Both transports are configured with ordinary fields, not raw JSON. Saving
  // a server must not connect to it; this temporary endpoint is never contacted.
  await page.getByRole('button', { name: '添加服务', exact: true }).click()
  await page.getByLabel('标识', { exact: true }).fill('temporary-http')
  await page.getByRole('combobox', { name: '连接方式', exact: true }).selectOption('http')
  await page.getByLabel('服务地址', { exact: true }).fill('https://example.invalid/mcp')
  await page.getByRole('button', { name: '保存服务', exact: true }).click()
  await expect(page.getByRole('button', { name: '保存服务', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: '编辑 temporary-http', exact: true }).click()
  await expect(page.getByLabel('服务地址', { exact: true })).toHaveValue(
    'https://example.invalid/mcp',
  )
  await page.getByLabel('显示名称', { exact: true }).fill('Temporary HTTP')
  await page.getByRole('button', { name: '保存服务', exact: true }).click()
  await expect(page.getByRole('button', { name: '保存服务', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: '删除 Temporary HTTP', exact: true }).click()
  await page.getByRole('button', { name: '移除', exact: true }).click()
  await expect(page.getByText('Temporary HTTP', { exact: true })).toHaveCount(0)
  await mkdir(resolve('test-results'), { recursive: true })
  await page.screenshot({
    path: resolve('test-results/desktop-extensions-settings.png'),
    fullPage: true,
  })
  const run = await page.evaluate(
    ({ root, model }) =>
      window.harness.createRun({
        prompt: 'DOCUMENTS',
        model_id: model,
        workspace_path: root,
        permission_mode: 'read_only',
      }),
    { root, model: configured.model.id },
  )
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, run.id)
  const documents = await settled(page, run.id)
  await page.locator('.run-skills > summary').click()
  await expect(page.getByText('review', { exact: true })).toBeVisible()
  await expect(page.getByText('global-review', { exact: true })).toBeVisible()
  const addedSkill = join(project, '.agents/skills/later')
  await mkdir(addedSkill)
  await writeFile(
    join(addedSkill, 'SKILL.md'),
    '---\nname: later\ndescription: Installed during a run\n---\nUse the fresh catalog.\n',
  )
  await page.locator('.run-skills').getByRole('button', { name: '刷新', exact: true }).click()
  await expect(page.getByText('later', { exact: true })).toBeVisible()
  await page.locator('.run-skills > summary').click()
  assert(
    documents.messages
      .filter((message) => message.role === 'tool')
      .every((message) => !message.is_error),
  )
  const text = JSON.stringify(documents.messages)
  assert(text.includes('面试手册'))
  assert(text.includes('experienced Rust engineer'))
  assert(text.includes('RESOURCE_BODY_MARKER'))
  const first = modelRequests[0]
  assert(!JSON.stringify(first).includes('SKILL_BODY_MARKER'), 'skill body must be lazy')
  const names = first.tools.map((tool) => tool.function.name)
  for (const name of ['read_document', 'search_document', 'list_skills', 'call_mcp_tool'])
    assert(names.includes(name))
  for (const name of ['run_command', 'write_file', 'edit_file']) assert(!names.includes(name))
  await page.evaluate((id) => window.harness.submitInput(id, 'MCP'), run.id)
  await expect(page.getByRole('button', { name: '允许本次', exact: true })).toBeVisible({
    timeout: 20_000,
  })
  await page.screenshot({ path: resolve('test-results/desktop-mcp-approval.png') })
  await page.getByRole('button', { name: '允许本次', exact: true }).click()
  await approval(page, run.id, true)
  const mcp = await settled(page, run.id)
  const outputs = mcp.messages
    .filter((message) => message.tool_name === 'call_mcp_tool')
    .map((message) => JSON.parse(message.text))
  assert.deepEqual(
    outputs.map((output) => output.structuredContent.calls),
    [1, 2],
    'stateful calls share one MCP process',
  )
  await page.evaluate((id) => window.harness.submitInput(id, 'RESOURCES'), run.id)
  await approval(page, run.id, true)
  await approval(page, run.id, true)
  const resources = await settled(page, run.id)
  assert(JSON.stringify(resources).includes('MCP_RESOURCE_OK'))
  assert(JSON.stringify(resources).includes('MCP_PROMPT_OK'))
  const beforeDenied = await readFile(callsFile, 'utf8')
  await page.evaluate((id) => window.harness.submitInput(id, 'DENY'), run.id)
  await approval(page, run.id, false)
  assert(
    (await settled(page, run.id)).messages.some(
      (message) => message.is_error && message.text.includes('denied'),
    ),
  )
  assert.equal(await readFile(callsFile, 'utf8'), beforeDenied)
  await page.evaluate((id) => window.harness.submitInput(id, 'CANCEL'), run.id)
  await expect
    .poll(
      async () =>
        (await page.evaluate((id) => window.harness.runExtensions(id), run.id)).approvals.length,
    )
    .toBe(1)
  await page.evaluate((id) => window.harness.cancelRun(id), run.id)
  assert.equal(
    (await page.evaluate((id) => window.harness.runExtensions(id), run.id)).approvals.length,
    0,
  )
  assert.equal(await readFile(callsFile, 'utf8'), beforeDenied)
  await page.evaluate(async () => {
    const { config } = await window.harness.extensionSettings()
    return window.harness.saveExtensions({
      ...config,
      mcp_servers: config.mcp_servers.map((server) => ({ ...server, enabled: false })),
      disabled_skill_ids: ['review'],
    })
  })
  await application.close()
  application = undefined
  page = await launch()
  const settings = await page.evaluate(() => window.harness.extensionSettings())
  assert.equal(settings.config.mcp_servers[0].enabled, false)
  assert.deepEqual(settings.config.disabled_skill_ids, ['review'])
  const projectState = await page.evaluate((id) => window.harness.runExtensions(id), run.id)
  assert.equal(projectState.skills.find((skill) => skill.id === 'review').enabled, false)
  assert.deepEqual(projectState.approvals, [])
  assert.deepEqual(errors, [])
  console.log(
    JSON.stringify(
      {
        passed: true,
        directory,
        run: run.id,
        checks: [
          'readonly PDF/DOCX',
          'lazy skills/resources',
          'MCP discovery',
          'stateful calls',
          'resources/prompts',
          'approval/denial/cancellation',
          'restart/disable',
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
