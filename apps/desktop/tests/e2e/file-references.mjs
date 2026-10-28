import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-file-e2e-'))
const dataDirectory = join(directory, 'app-data')
const artifacts = resolve('test-results')
await mkdir(artifacts, { recursive: true })
const firstPath = join(directory, '计划 sample.ts')
const secondPath = join(directory, 'clipboard-notes.md')
const thirdPath = join(directory, 'resume.txt')
const unsupportedPath = join(directory, 'unsupported.zip')
const largePath = join(directory, 'too-large.txt')
const firstText = 'export const plan = "CREATE_FILE_VERSION_1";\n// 发送时保留这个快照。\n'
const secondText = '# PASTE_FILE_SNAPSHOT\nThese notes came from the native file clipboard.\n'
const thirdText = 'RESUME_FILE_SNAPSHOT\nContinue from the recorded state.\n'
const pdfFiles = [
  { name: '选择 report.PDF', token: 'SELECTED_PDF_SNAPSHOT' },
  { name: 'dropped-report.pdf', token: 'DROPPED_PDF_SNAPSHOT' },
  { name: 'pasted-report.pdf', token: 'PASTED_PDF_SNAPSHOT' },
].map((file) => ({ ...file, path: join(directory, file.name) }))
const changedPdfToken = 'CHANGED_PDF_MUST_NOT_REPLACE_IMPORTED_SNAPSHOT'

// Every PDF byte is synthetic; the fixture never needs a user's documents.
function minimalPdf(text) {
  const escaped = text.replaceAll('\\', '\\\\').replaceAll('(', '\\(').replaceAll(')', '\\)')
  const content = `BT /F1 12 Tf 20 100 Td (${escaped}) Tj ET`
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    `<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}\nendstream`,
  ]
  let pdf = '%PDF-1.4\n'
  const offsets = []
  for (const [index, object] of objects.entries()) {
    offsets.push(Buffer.byteLength(pdf))
    pdf += `${index + 1} 0 obj\n${object}\nendobj\n`
  }
  const xref = Buffer.byteLength(pdf)
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`
  for (const offset of offsets) pdf += `${String(offset).padStart(10, '0')} 00000 n \n`
  pdf += `trailer\n<< /Root 1 0 R /Size ${objects.length + 1} >>\nstartxref\n${xref}\n%%EOF\n`
  return Buffer.from(pdf)
}

await Promise.all([
  writeFile(firstPath, firstText),
  writeFile(secondPath, secondText),
  writeFile(thirdPath, thirdText),
  writeFile(unsupportedPath, Buffer.from([80, 75, 3, 4])),
  writeFile(largePath, 'x'.repeat(256 * 1024 + 1)),
  ...pdfFiles.map((file) => writeFile(file.path, minimalPdf(file.token))),
])

const calls = []
const errors = []
const fixtureErrors = []
let holdNext = false
const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end(JSON.stringify({ data: [] }))
      return
    }
    assert.equal(request.method, 'POST')
    assert.equal(request.url, '/v1/chat/completions')
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    if (holdNext) {
      holdNext = false
      return
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.write(
      `data: ${JSON.stringify({
        id: `reference-${calls.length}`,
        choices: [
          {
            index: 0,
            delta: { role: 'assistant', content: 'The referenced snapshot was received.' },
            finish_reason: 'stop',
          },
        ],
      })}\n\n`,
    )
    response.write(
      `data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 500, completion_tokens: 20 } })}\n\n`,
    )
    response.end('data: [DONE]\n\n')
  } catch (error) {
    fixtureErrors.push(error.message)
    response.writeHead(500)
    response.end('Fixture failed')
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const configuration = {
  protocol: 'chat_completions',
  base_url: `http://127.0.0.1:${server.address().port}/v1`,
  model: 'file-reference-fixture',
  api_key: 'file-reference-fixture-key',
  context_window: 64000,
  max_output_tokens: 2048,
  reasoning_mode: 'off',
  reasoning_effort: 'medium',
}
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: dataDirectory }
delete env.ELECTRON_RUN_AS_NODE
let application
let clipboardBackedUp = false

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
  await page.evaluate(() => window.harness.config())
  return page
}

async function clipboardBackup() {
  await application.evaluate(async ({ clipboard, ClipboardItem }) => {
    // read() returns lazy handles. Materialize every payload before changing
    // the OS clipboard; nothing from the user's clipboard leaves this process.
    const backup = []
    for (const item of await clipboard.read()) {
      const values = {}
      for (const type of item.types) values[type] = await item.getType(type)
      // Some hosts return an empty item for an empty clipboard.
      if (item.types.length) backup.push(new ClipboardItem(values))
    }
    globalThis.__fileReferenceClipboardBackup = backup
  })
  clipboardBackedUp = true
}

async function restoreClipboard() {
  if (!application || !clipboardBackedUp) return
  await application.evaluate(async ({ clipboard }) => {
    await clipboard.write(globalThis.__fileReferenceClipboardBackup)
    delete globalThis.__fileReferenceClipboardBackup
  })
  clipboardBackedUp = false
}

async function closeApplication() {
  await restoreClipboard()
  await application?.close()
  application = undefined
}

async function diskFileEvent(page, paths, kind = 'drop') {
  await page.evaluate(() => {
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    input.hidden = true
    input.id = 'file-reference-test-input'
    document.body.append(input)
  })
  const input = page.locator('#file-reference-test-input')
  await input.setInputFiles(paths)
  await input.evaluate((input, kind) => {
    const data = new DataTransfer()
    for (const file of input.files) data.items.add(file)
    if (kind === 'drop') {
      const composer = document.querySelector('.composer')
      composer.dispatchEvent(
        new DragEvent('dragenter', { dataTransfer: data, bubbles: true, cancelable: true }),
      )
      composer.dispatchEvent(
        new DragEvent('drop', { dataTransfer: data, bubbles: true, cancelable: true }),
      )
    } else {
      document
        .querySelector('.composer textarea')
        .dispatchEvent(
          new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }),
        )
    }
    input.remove()
  }, kind)
}

async function settled(page, id, status = 'waiting_for_input') {
  await expect
    .poll(() => page.evaluate(async (id) => (await window.harness.getRun(id)).status, id), {
      timeout: 20_000,
    })
    .toBe(status)
  return page.evaluate((id) => window.harness.getRun(id), id)
}

function userBodies(request) {
  return request.messages
    .filter((message) => message.role === 'user')
    .map((message) =>
      typeof message.content === 'string' ? message.content : JSON.stringify(message.content),
    )
    .join('\n')
}

async function previewPdfSnapshot(page) {
  const file = pdfFiles[0]
  await page.getByRole('button', { name: `预览文件 ${file.name}`, exact: true }).click()
  const preview = page.getByRole('dialog', { name: file.name, exact: true })
  await expect(preview.getByRole('button', { name: '另存为', exact: true })).toBeVisible()
  const saved = await page.evaluate(async (name) => {
    const id = window.location.hash.split('/runs/')[1]
    const run = await window.harness.getRun(id)
    const message = run.messages.find((message) =>
      message.files?.some((file) => file.name === name),
    )
    const resource = await window.harness.resolveResource({
      runId: id,
      messageId: message.id,
      fileIndex: message.files.findIndex((file) => file.name === name),
    })
    return {
      kind: resource.kind,
      mimeType: resource.mimeType,
      name: resource.name,
    }
  }, file.name)
  assert.equal(saved.kind, 'file')
  assert.equal(saved.mimeType, 'application/pdf')
  const savedPath = join(directory, `saved-${file.name}`)
  await application.evaluate(({ dialog }, path) => {
    dialog.showSaveDialog = async (...args) => {
      globalThis.__attachmentExportName = args.at(-1).defaultPath
      return { canceled: false, filePath: path }
    }
  }, savedPath)
  await preview.getByRole('button', { name: '另存为', exact: true }).click()
  await expect
    .poll(async () => [...(await readFile(savedPath).catch(() => Buffer.alloc(0)))])
    .toEqual([...minimalPdf(file.token)])
  assert.equal(await application.evaluate(() => globalThis.__attachmentExportName), file.name)
  await expect(preview).not.toContainText(changedPdfToken)
  await preview.getByRole('button', { name: '关闭预览', exact: true }).click()
  await expect(preview).toHaveCount(0)
}

try {
  let page = await launch()
  // This fixture counts attachment/resume requests, not Wiki compilation calls.
  await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
  const configured = await page.evaluate(
    (input) => window.harness.configureModel(input),
    configuration,
  )
  await page.evaluate((selectedModel) => {
    localStorage.setItem(
      'open-harness-workspace',
      JSON.stringify({
        state: { drafts: {}, selectedModel, inspectorOpen: false, inspectorTab: 'context' },
        version: 1,
      }),
    )
  }, configured.model.id)
  await page.reload()
  const draftFiles = () => page.locator('.composer .file-references li')
  const addContent = page.getByRole('button', { name: '添加内容', exact: true })
  await expect(addContent).toBeVisible()
  await addContent.focus()
  await expect(page.getByRole('tooltip')).toContainText('添加文件或使用技能')
  const tooltipId = await page.getByRole('tooltip').getAttribute('id')
  assert(tooltipId)
  await expect(addContent).toHaveAttribute('aria-describedby', tooltipId)
  await application.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] })
  }, firstPath)
  await addContent.click()
  await page.getByRole('menuitem', { name: '添加文件', exact: true }).click()
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles()).toContainText('计划 sample.ts')
  await expect(page.locator('.composer')).not.toHaveClass(/composer-dragging/)
  await diskFileEvent(page, [firstPath])
  await expect(draftFiles()).toHaveCount(1)
  await page.getByRole('button', { name: '移除文件 计划 sample.ts', exact: true }).click()
  await expect(draftFiles()).toHaveCount(0)
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeDisabled()
  await diskFileEvent(page, [firstPath])
  await page.getByRole('link', { name: '设置', exact: true }).click()
  await page.getByRole('link', { name: '会话', exact: true }).click()
  await expect(draftFiles()).toHaveCount(1)

  await diskFileEvent(page, [unsupportedPath])
  await expect(page.getByRole('alert')).toHaveCount(0)
  await expect(draftFiles()).toHaveCount(2)
  await page.getByRole('button', { name: '移除文件 unsupported.zip', exact: true }).click()
  await diskFileEvent(page, [largePath])
  await expect(page.getByRole('alert')).toHaveCount(0)
  await expect(draftFiles()).toHaveCount(2)
  await page.getByRole('button', { name: '移除文件 too-large.txt', exact: true }).click()
  await diskFileEvent(page, [firstPath])
  await expect(page.getByRole('alert')).toHaveCount(0)
  assert(
    !(await page.evaluate(() => localStorage.getItem('open-harness-workspace'))).includes(
      'CREATE_FILE_VERSION_1',
    ),
    'unsent file contents must not enter localStorage',
  )
  await page.screenshot({ path: join(artifacts, 'desktop-file-references-draft.png') })

  // An admission error keeps both the typed message and imported snapshot.
  const description = page.getByRole('textbox', { name: '任务描述', exact: true })
  const invalidDraft = 'x'.repeat(4001)
  await description.fill(invalidDraft)
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('4,000')
  await expect(description).toHaveValue(invalidDraft)
  await expect(draftFiles()).toHaveCount(1)
  assert.equal((await page.evaluate(() => window.harness.listRuns())).length, 0)
  await description.fill('')
  await expect(page.getByRole('button', { name: '开始运行', exact: true })).toBeEnabled()
  await writeFile(firstPath, 'CREATE_FILE_VERSION_2 should not replace the imported snapshot.\n')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => page.evaluate(() => window.location.hash)).toMatch(/\/runs\//)
  const id = await page.evaluate(() => window.location.hash.split('/runs/')[1])
  let detail = await settled(page, id)
  assert.equal(detail.title, '计划 sample.ts')
  assert.equal(detail.prompt, '')
  assert.equal(detail.workspace_path, undefined)
  assert.equal(detail.messages[0].files[0].size, Buffer.byteLength(firstText))
  assert(!JSON.stringify(detail).includes('CREATE_FILE_VERSION_1'))
  assert(!userBodies(calls[0]).includes('CREATE_FILE_VERSION_1'))
  assert(userBodies(calls[0]).includes('attachment:'))
  const firstSaved = await page.evaluate(async (id) => {
    const run = await window.harness.getRun(id)
    const resource = await window.harness.resolveResource({
      runId: id,
      messageId: run.messages[0].id,
      fileIndex: 0,
    })
    return resource.text
  }, id)
  assert.equal(firstSaved, firstText)
  assert(!userBodies(calls[0]).includes('CREATE_FILE_VERSION_2'))
  await expect(draftFiles()).toHaveCount(0)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(1)

  await clipboardBackup()
  await application.evaluate(async ({ clipboard, ClipboardItem }, uri) => {
    await clipboard.write([new ClipboardItem({ 'text/uri-list': uri })])
  }, pathToFileURL(secondPath).href)
  await page.getByRole('textbox', { name: '任务描述', exact: true }).focus()
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(draftFiles()).toHaveCount(1)
  await expect(draftFiles()).toContainText('clipboard-notes.md')
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(draftFiles()).toHaveCount(1)

  await application.evaluate(async ({ clipboard }) =>
    clipboard.writeText('普通粘贴文本 NORMAL_PASTE'),
  )
  const followup = page.getByRole('textbox', { name: '任务描述', exact: true })
  await followup.fill('prefix ')
  await followup.press('End')
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(followup).toHaveValue('prefix 普通粘贴文本 NORMAL_PASTE')
  assert.equal(
    await followup.evaluate((textarea) => {
      const clipboardData = new DataTransfer()
      clipboardData.setData('text/plain', 'https://example.com/reference')
      clipboardData.setData('text/uri-list', 'https://example.com/reference')
      const event = new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true })
      textarea.dispatchEvent(event)
      return event.defaultPrevented
    }),
    false,
    'ordinary web links must retain native paste even with a URI-list representation',
  )

  // Exercise every attachment entry point against the real original-file store.
  await application.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] })
  }, pdfFiles[0].path)
  await addContent.click()
  await page.getByRole('menuitem', { name: '添加文件', exact: true }).click()
  await expect(draftFiles()).toHaveCount(2)
  await expect(draftFiles().last()).toContainText(pdfFiles[0].name)
  await diskFileEvent(page, [pdfFiles[1].path])
  await expect(draftFiles()).toHaveCount(3)
  await expect(draftFiles().last()).toContainText(pdfFiles[1].name)
  await diskFileEvent(page, [pdfFiles[2].path], 'paste')
  await expect(draftFiles()).toHaveCount(4)
  await expect(draftFiles().last()).toContainText(pdfFiles[2].name)
  await page.getByRole('button', { name: `移除文件 ${pdfFiles[2].name}`, exact: true }).click()
  await expect(draftFiles()).toHaveCount(3)
  await application.evaluate(async ({ clipboard, ClipboardItem }, uri) => {
    await clipboard.write([new ClipboardItem({ 'text/uri-list': uri })])
  }, pathToFileURL(pdfFiles[2].path).href)
  await followup.focus()
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+v' : 'Control+v')
  await expect(draftFiles()).toHaveCount(4)
  await expect(draftFiles().last()).toContainText(pdfFiles[2].name)
  await expect(followup).toHaveValue('prefix 普通粘贴文本 NORMAL_PASTE')
  await expect(page.getByRole('alert')).toHaveCount(0)
  const unsentWorkspace = await page.evaluate(() => localStorage.getItem('open-harness-workspace'))
  for (const file of pdfFiles)
    assert(!unsentWorkspace.includes(file.token), 'unsent PDF contents must not enter localStorage')
  await Promise.all(pdfFiles.map((file) => writeFile(file.path, minimalPdf(changedPdfToken))))

  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(2)
  detail = await settled(page, id)
  assert.equal(
    detail.messages.filter((message) => message.role === 'user')[1].files[0].name,
    'clipboard-notes.md',
  )
  assert(!userBodies(calls[1]).includes('PASTE_FILE_SNAPSHOT'))
  const importedPdfs = detail.messages
    .filter((message) => message.role === 'user')[1]
    .files.slice(1)
  assert.deepEqual(
    importedPdfs.map((file) => file.name),
    pdfFiles.map((file) => file.name),
  )
  const pdfBody = userBodies(calls[1])
  for (const [index, file] of pdfFiles.entries()) {
    assert(!pdfBody.includes(file.token), `PDF body must be read on demand: ${file.name}`)
    assert(pdfBody.includes(file.name))
    assert(importedPdfs[index].size > Buffer.byteLength(file.token))
    assert(!JSON.stringify(detail).includes(file.token), 'run detail must return PDF metadata only')
  }
  assert(pdfBody.includes('attachment:'), 'the model needs a tool-readable original reference')
  assert(!pdfBody.includes('%PDF-1.4'), 'the provider must receive text, not raw PDF bytes')
  assert(!pdfBody.includes(changedPdfToken), 'sending must retain the imported PDF snapshot')
  await previewPdfSnapshot(page)

  holdNext = true
  await followup.fill('Pause this request so the next message exercises resume.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(3)
  await page.getByRole('button', { name: '停止运行', exact: true }).click()
  await settled(page, id, 'cancelled')
  await diskFileEvent(page, [thirdPath], 'paste')
  await expect(draftFiles()).toContainText('resume.txt')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(4)
  detail = await settled(page, id)
  assert(detail.events.some((event) => event.type === 'run_resumed'))
  assert(!userBodies(calls.at(-1)).includes('RESUME_FILE_SNAPSHOT'))
  assert(userBodies(calls.at(-1)).includes('resume.txt'))
  await expect(page.locator('.message-user .file-references li')).toHaveCount(6)
  await page.screenshot({ path: join(artifacts, 'desktop-file-references-conversation.png') })
  await closeApplication()

  page = await launch()
  const config = await page.evaluate(() => window.harness.config())
  if (!config.models.find((model) => model.id === configured.model.id)?.available)
    await page.evaluate((input) => window.harness.configureModel(input), configuration)
  await page.evaluate((id) => {
    window.location.hash = `/runs/${id}`
  }, id)
  detail = await settled(page, id)
  await expect(page.locator('.message-user .file-references li')).toHaveCount(6)
  await previewPdfSnapshot(page)
  assert.equal(
    detail.messages.find((message) => message.role === 'user').files[0].size,
    Buffer.byteLength(firstText),
  )
  await page
    .getByRole('textbox', { name: '任务描述', exact: true })
    .fill('Check the saved snapshots after restart.')
  await page.getByRole('button', { name: '开始运行', exact: true }).click()
  await expect.poll(() => calls.length).toBe(5)
  await settled(page, id)
  const afterRestart = userBodies(calls.at(-1))
  for (const token of [
    'CREATE_FILE_VERSION_1',
    'PASTE_FILE_SNAPSHOT',
    'RESUME_FILE_SNAPSHOT',
    ...pdfFiles.map((file) => file.token),
  ])
    assert(!afterRestart.includes(token), `original bytes must remain outside the prompt: ${token}`)
  assert(!afterRestart.includes('CREATE_FILE_VERSION_2'))
  assert(!afterRestart.includes(changedPdfToken))
  assert(!afterRestart.includes('%PDF-1.4'))
  const events = await readFile(
    join(dataDirectory, 'runtime', 'events', `run-${Buffer.from(id).toString('hex')}.jsonl`),
    'utf8',
  )
  assert(!events.includes('CREATE_FILE_VERSION_1'))
  assert(events.includes('attachment'))
  assert(!events.includes('CREATE_FILE_VERSION_2'))
  for (const file of pdfFiles) assert(!events.includes(file.token))
  assert(!events.includes(changedPdfToken))
  assert.deepEqual(errors, [], 'renderer must not report uncaught errors')
  assert.deepEqual(fixtureErrors, [])
  console.log(
    JSON.stringify({
      result: 'passed',
      run_id: id,
      provider_requests: calls.length,
      snapshots: 6,
      data_directory: dataDirectory,
    }),
  )
} finally {
  await closeApplication()
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
}
