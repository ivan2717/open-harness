import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { access, mkdtemp, mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative, resolve, sep, win32 } from 'node:path'
import { createRequire } from 'node:module'
import { _electron, expect } from '@playwright/test'

// Local provider fixture drives real file and command tools through the packaged
// application. No model API or personal project is used by this test.
const require = createRequire(import.meta.url)
const directory = await mkdtemp(join(tmpdir(), 'openharness-script-workflow-'))
const artifacts = resolve(process.env.OPENHARNESS_E2E_ARTIFACTS ?? 'test-results/scripts')
await mkdir(artifacts, { recursive: true })
const counts = new Map()
const failures = []
const calls = []
let responseNumber = 0
let dependencies
const png =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq1sAAAAASUVORK5CYII='
const identityEnvironmentKeys = ['HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA']
const temporaryEnvironmentKeys = ['TMPDIR', 'TMP', 'TEMP']
const dependencyEnvironmentKeys = [
  'PIP_CACHE_DIR',
  'PIP_REQUIRE_VIRTUALENV',
  'PNPM_HOME',
  'pnpm_config_cache_dir',
  'pnpm_config_store_dir',
  'pnpm_config_state_dir',
  'pnpm_config_config_dir',
  'pnpm_config_global_dir',
  'pnpm_config_global_bin_dir',
  'npm_config_userconfig',
  'npm_config_globalconfig',
  'UV_CACHE_DIR',
  'UV_PROJECT_ENVIRONMENT',
  'OPENHARNESS_TASK_DIRECTORY',
]
const inspectedEnvironmentKeys = [
  ...identityEnvironmentKeys,
  ...temporaryEnvironmentKeys,
  ...dependencyEnvironmentKeys,
  'NODE_PATH',
]
const pythonImports = [
  'PIL',
  'docx',
  'pptx',
  'openpyxl',
  'pypdf',
  'reportlab',
  'numpy',
  'pandas',
  'matplotlib',
]
const managedPythonScript = `import importlib,json,sys,tempfile
for name in ${JSON.stringify(pythonImports)}:
 importlib.import_module(name)
from docx import Document
from reportlab.pdfgen.canvas import Canvas
from pypdf import PdfReader
marker = 'MANAGED_PYTHON_OK'
document = Document()
document.add_paragraph(marker)
document.save('outputs/runtime.docx')
canvas = Canvas('outputs/runtime.pdf')
canvas.drawString(72, 720, marker)
canvas.save()
assert Document('outputs/runtime.docx').paragraphs[0].text == marker
assert marker in PdfReader('outputs/runtime.pdf').pages[0].extract_text()
assert sys.prefix == sys.base_prefix
with tempfile.TemporaryFile() as scratch:
 scratch.write(b'SHARED_RUNTIME_OK')
 scratch.seek(0)
 assert scratch.read() == b'SHARED_RUNTIME_OK'
import numpy as np, pandas as pd, matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
frame = pd.DataFrame({'value': np.array([1, 2, 3])})
frame.to_csv('outputs/science.csv', index=False)
assert pd.read_csv('outputs/science.csv')['value'].sum() == 6
plt.plot(frame['value'])
plt.savefig('outputs/science.png')
plt.close()
print(json.dumps({'executable':sys.executable,'prefix':sys.prefix,'base_prefix':sys.base_prefix,'imports':${JSON.stringify(pythonImports)},'artifact_text':marker,'temporary_directory':tempfile.gettempdir()}))`

function outputScript() {
  const runtime = dependencies?.runtime ?? dependencies
  const python = runtime.python
  assert.equal(dependencies.dependency_environment, undefined)
  if (packaged) assert.equal(python?.source, 'bundled', 'packaged scripts must use managed Python')
  return [
    'if (process.versions.electron) throw new Error("Scripts must use standalone Node, not Electron");',
    "const fs = require('node:fs');",
    "const path = require('node:path');",
    "const { spawnSync } = require('node:child_process');",
    "fs.mkdirSync('outputs', { recursive: true });",
    "fs.writeFileSync('outputs/report.csv', 'name,total\\nexample,42\\n');",
    `fs.writeFileSync('outputs/plot.png', Buffer.from('${png}', 'base64'));`,
    `const environment = Object.fromEntries(${JSON.stringify(inspectedEnvironmentKeys)}.map(key => [key, process.env[key]]));`,
    `const python = ${JSON.stringify(python ?? null)};`,
    'const sharedPythonExecutable = python?.executable;',
    `const manager = ${JSON.stringify(runtime.node.package_manager ?? null)};`,
    'function run(executable, args, environment = {}) {',
    '  const child = spawnSync(executable, args, { encoding: "utf8", timeout: 60000, env: {...process.env, ...environment} });',
    '  if (child.error || child.status !== 0) throw new Error("Runtime failed: " + (child.error || child.stderr));',
    '  return child.stdout;',
    '}',
    'let pythonReport = null;',
    'if (python) {',
    `  const source = python.source === 'bundled' ? ${JSON.stringify(managedPythonScript)} : 'import json,sys; print(json.dumps({"executable":sys.executable,"prefix":sys.prefix,"base_prefix":sys.base_prefix,"imports":[]}))';`,
    '  pythonReport = JSON.parse(run(python.executable, [...(python.args || []), "-B", "-c", source], python.environment));',
    '  pythonReport.pip = run(python.executable, [...(python.args || []), "-m", "pip", "--version"], python.environment).trim();',
    '}',
    'if (process.platform === "darwin" && python?.source === "bundled") {',
    '  const probe = path.join(path.dirname(sharedPythonExecutable), ".openharness-write-probe-" + process.pid);',
    '  let denied = false;',
    '  try { fs.writeFileSync(probe, "unexpected"); fs.unlinkSync(probe); } catch (error) { if (["EPERM", "EACCES"].includes(error.code)) denied = true; else throw error; }',
    '  if (!denied) throw new Error("macOS sandbox allowed modifying the managed Python runtime");',
    '}',
    'let pnpmVersion = null;',
    'if (manager) {',
    '  if (manager.name !== "pnpm") throw new Error("Expected the managed pnpm command");',
    '  pnpmVersion = run(manager.executable, [...manager.args, "--version"], manager.environment).trim();',
    '  const { stringify } = require("csv-stringify/sync");',
    '  const { parse } = require("csv-parse/sync");',
    '  if (parse(stringify([["managed", "42"]]))[0][1] !== "42") throw new Error("Bundled CSV libraries failed");',
    '}',
    'fs.writeFileSync("outputs/runtime-report.json", JSON.stringify({environment, node:{executable:process.execPath, version:process.versions.node, electron:process.versions.electron}, python:pythonReport, pnpm:pnpmVersion}));',
    "console.log('RUNTIME_OK ' + process.versions.node);",
  ].join('\n')
}

function patch(path, source) {
  return `*** Begin Patch\n*** Add File: ${path}\n${source
    .split('\n')
    .map((line) => `+${line}`)
    .join('\n')}\n*** End Patch`
}

function toolResults(body) {
  return body.messages
    .filter((message) => message.role === 'tool')
    .flatMap((message) => {
      try {
        return [JSON.parse(message.content)]
      } catch {
        return []
      }
    })
}

function invokeNode(path) {
  const runtime = dependencies?.runtime ?? dependencies
  assert(runtime?.node?.executable, 'runtime discovery must return a Node interpreter')
  const quote =
    process.platform === 'win32'
      ? (part) => `'${part.replaceAll("'", "''")}'`
      : (part) => `'${part.replaceAll("'", "'\\''")}'`
  return `${process.platform === 'win32' ? '& ' : ''}${[runtime.node.executable, ...(runtime.node.args ?? []), path].map(quote).join(' ')}`
}

function assertInside(parent, path, label) {
  assert.equal(typeof path, 'string', `${label} must be present`)
  assert(isAbsolute(path), `${label} must be absolute`)
  const normalize = (value) =>
    process.platform === 'win32' ? win32.toNamespacedPath(value).toLowerCase() : value
  const child = relative(normalize(parent), normalize(path))
  assert(
    child && child !== '..' && !child.startsWith(`..${sep}`) && !isAbsolute(child),
    `${label} must stay inside the task workspace: ${path}`,
  )
}

const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(404).end()
      return
    }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    const prompt = body.messages
      .filter(
        (message) =>
          message.role === 'user' &&
          ['SCRIPT_OUTPUT', 'SCRIPT_BACKGROUND', 'SCRIPT_AFTER_REVOKE'].includes(message.content),
      )
      .at(-1)?.content
    assert(prompt, 'request must retain the fixture task')
    const count = counts.get(prompt) ?? 0
    counts.set(prompt, count + 1)
    const results = toolResults(body)
    dependencies ??= results.find((result) => (result.runtime ?? result)?.node?.executable)
    let tool
    let text = '后台脚本正在运行，可以查看输出或停止。'
    if (prompt === 'SCRIPT_OUTPUT') {
      if (count === 0) tool = ['load_workspace_dependencies', {}]
      else if (count === 1)
        tool = ['apply_patch', { patch: patch('.openharness-tmp/report.cjs', outputScript()) }]
      else if (count === 2)
        tool = [
          'run_command',
          { command: invokeNode('.openharness-tmp/report.cjs'), yield_time_ms: 0 },
        ]
      else {
        const result = results.filter((item) => item.command_id).at(-1)
        assert(result, 'command tool must return its ID')
        if (result.status === 'running')
          tool = [
            'manage_command',
            {
              command_id: result.command_id,
              action: 'status',
              cursor: result.next_cursor,
              wait_ms: 1000,
            },
          ]
        else {
          assert.equal(result.status, 'completed', JSON.stringify(result))
          assert(
            results.some(
              (item) => item.command_id === result.command_id && item.output.includes('RUNTIME_OK'),
            ),
            JSON.stringify(result),
          )
          text = '脚本已完成。\n\n[下载报告](outputs/report.csv)\n\n![生成图片](outputs/plot.png)'
        }
      }
    } else if (prompt === 'SCRIPT_BACKGROUND') {
      if (count === 0)
        tool = [
          'apply_patch',
          {
            patch: patch(
              '.openharness-tmp/background.cjs',
              [
                "const { spawn } = require('node:child_process');",
                `spawn(process.execPath, ['-e', ${JSON.stringify("const fs = require('node:fs'); fs.writeFileSync('outputs/child-started', 'started'); setTimeout(() => fs.writeFileSync('outputs/child-escaped', 'escaped'), 5000);")}], { stdio: 'ignore' });`,
                "console.log('BACKGROUND_STARTED');",
                "setInterval(() => console.log('BACKGROUND_TICK'), 250);",
              ].join('\n'),
            ),
          },
        ]
      else if (count === 1)
        tool = [
          'run_command',
          { command: invokeNode('.openharness-tmp/background.cjs'), yield_time_ms: 0 },
        ]
    } else if (prompt === 'SCRIPT_AFTER_REVOKE') {
      if (count === 0)
        tool = ['run_command', { command: invokeNode('.openharness-tmp/report.cjs') }]
      else text = '命令已拒绝，未执行。'
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' })
    response.write(
      `data: ${JSON.stringify({
        id: `script-response-${++responseNumber}`,
        choices: [
          {
            index: 0,
            delta: tool
              ? {
                  role: 'assistant',
                  tool_calls: [
                    {
                      index: 0,
                      id: `script-call-${responseNumber}`,
                      type: 'function',
                      function: { name: tool[0], arguments: JSON.stringify(tool[1]) },
                    },
                  ],
                }
              : { role: 'assistant', content: text },
            finish_reason: tool ? 'tool_calls' : 'stop',
          },
        ],
      })}\n\n`,
    )
    response.write('data: [DONE]\n\n')
    response.end()
  } catch (error) {
    failures.push(error.stack ?? String(error))
    response.writeHead(500, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ error: 'Script fixture assertion failed' }))
  }
})
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
const env = { ...process.env, OPENHARNESS_DESKTOP_DATA_DIR: join(directory, 'data') }
delete env.ELECTRON_RUN_AS_NODE
const packaged = process.env.OPENHARNESS_E2E_EXECUTABLE
let application
let applicationPid
let currentStage = 'starting fixture'
const applicationLogs = []
const report = { directory, artifacts, packaged: Boolean(packaged), passed: false }

async function within(label, operation, timeout = 30_000) {
  let timer
  try {
    return await Promise.race([
      operation(),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${timeout} ms`)),
          timeout,
        )
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

async function saveDiagnostics() {
  await writeFile(
    join(artifacts, 'report.json'),
    JSON.stringify({ ...report, stage: currentStage, failures }, null, 2),
  )
  await writeFile(join(artifacts, 'application.log'), applicationLogs.join('\n'))
}

async function checkpoint(stage) {
  currentStage = stage
  console.log(`[script-workflow] ${stage}`)
  await saveDiagnostics()
}

function record(source, text) {
  if (applicationLogs.length < 500)
    applicationLogs.push(`[${source}] ${String(text).slice(0, 16_384)}`)
}

async function closeApplication() {
  if (!application) return
  const child = application.process()
  try {
    await within('test application shutdown', () => application.close(), 10_000)
  } catch (error) {
    record('cleanup', error.message)
    // Only this live ChildProcess was launched by this fixture. Never discover
    // or terminate an app by name, nor reuse a PID from a previous test run.
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGKILL')
      await within(
        'test process exit',
        () =>
          new Promise((resolve) => {
            if (child.exitCode !== null || child.signalCode !== null) resolve()
            else child.once('exit', resolve)
          }),
        5_000,
      ).catch((error) => record('cleanup', error.message))
    }
  }
}

try {
  await within(
    'script workflow',
    async () => {
      await checkpoint('launch application')
      application = await _electron.launch({
        executablePath: packaged ?? require('electron'),
        args: packaged ? [] : [resolve('.')],
        env,
        timeout: 60_000,
      })
      applicationPid = application.process().pid
      report.applicationPid = applicationPid
      application.process().stdout?.on('data', (data) => record('stdout', data))
      application.process().stderr?.on('data', (data) => record('stderr', data))
      await checkpoint('wait for window')
      const page = await within('first application window', () => application.firstWindow())
      page.setDefaultTimeout(15_000)
      const errors = []
      page.on('pageerror', (error) => errors.push(error.message))
      page.on('console', (message) => record(`renderer:${message.type()}`, message.text()))
      await expect(page.getByRole('heading', { name: '开始一个新任务' })).toBeVisible()
      await checkpoint('prepare fixture model')
      // This test uses a fake API key in an isolated temporary profile. Exercise the
      // supported session-only vault path instead of prompting macOS Keychain for
      // the packaged app's real encryption key. Production behavior is unchanged.
      await within('session-only fixture credentials', () =>
        application.evaluate(({ safeStorage }) => {
          safeStorage.isEncryptionAvailable = () => false
        }),
      )
      await page.evaluate(() => window.harness.setPluginEnabled('builtin.llm-wiki', false))
      const configured = await within('configure fixture model', () =>
        page.evaluate(
          (base_url) =>
            window.harness.configureModel({
              protocol: 'chat_completions',
              base_url,
              api_key: 'script-fixture',
              model: 'script-fixture',
              context_window: 128000,
              max_output_tokens: 2048,
              reasoning_mode: 'off',
              reasoning_effort: 'medium',
            }),
          `http://127.0.0.1:${server.address().port}/v1`,
        ),
      )
      await page.reload()
      await expect(page.getByRole('button', { name: '选择模型', exact: true })).toContainText(
        'script-fixture',
      )
      await checkpoint('create script conversation')
      const run = await page.evaluate(
        (model_id) => window.harness.createRun({ model_id, prompt: 'SCRIPT_OUTPUT' }),
        configured.model.id,
      )
      assert.equal(run.workspace_path ?? null, null)
      assert(run.execution_directory, 'ordinary chat must have a working directory')
      report.runId = run.id
      await page.evaluate((id) => {
        window.location.hash = `/runs/${id}`
      }, run.id)
      await expect(page.getByRole('button', { name: '打开会话文件夹', exact: true })).toBeVisible()
      await expect(page.getByRole('button', { name: '允许本次', exact: true })).toBeVisible({
        timeout: 30_000,
      })
      await checkpoint('approve script execution')
      await page.screenshot({ path: join(artifacts, 'script-approval.png') })
      await page.getByRole('button', { name: '本会话同范围允许', exact: true }).click()
      await expect
        .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), run.id)).status, {
          timeout: 30_000,
        })
        .toBe('waiting_for_input')
      assert.deepEqual(failures, [])
      await checkpoint('verify generated outputs')
      assert.equal(
        await readFile(join(run.execution_directory, 'outputs/report.csv'), 'utf8'),
        'name,total\nexample,42\n',
      )
      await expect(page.getByRole('button', { name: '下载报告', exact: true })).toBeEnabled()
      await page.getByRole('button', { name: '下载报告', exact: true }).click()
      await expect(page.getByRole('dialog')).toContainText('example,42')
      await page.keyboard.press('Escape')
      await expect(page.getByRole('img', { name: 'plot.png', exact: true })).toBeVisible()
      await expect
        .poll(() =>
          page
            .getByRole('img', { name: 'plot.png', exact: true })
            .evaluate((image) => image.complete && image.naturalWidth > 0),
        )
        .toBe(true)
      await page.screenshot({ path: join(artifacts, 'script-generated-files.png') })
      const review = await page.evaluate((id) => window.harness.getWorkspace(id), run.id)
      assert.equal(review.root, run.execution_directory)
      assert.equal(review.commands.at(-1).status, 'completed')
      assert(review.commands.at(-1).output.includes('RUNTIME_OK'))
      await checkpoint('verify shared runtime without automatic dependency environments')
      const runtimeReport = JSON.parse(
        await readFile(join(run.execution_directory, 'outputs/runtime-report.json'), 'utf8'),
      )
      for (const key of identityEnvironmentKeys)
        if (env[key])
          assert.equal(
            runtimeReport.environment[key],
            env[key],
            `${key} must retain its host value`,
          )
      for (const key of temporaryEnvironmentKeys)
        assertInside(run.execution_directory, runtimeReport.environment[key], key)
      for (const key of dependencyEnvironmentKeys)
        assert.equal(
          runtimeReport.environment[key],
          undefined,
          `${key} must not force a per-conversation dependency environment`,
        )
      const runtime = dependencies?.runtime ?? dependencies
      assert.equal(runtimeReport.node.executable, runtime.node.executable)
      assert.equal(runtimeReport.node.electron, undefined)
      assert.match(runtimeReport.node.version, /^24\./)
      assert.equal(dependencies.dependency_environment, undefined)
      assert.equal(
        runtimeReport.environment.NODE_PATH,
        (runtime.node.module_paths ?? []).join(process.platform === 'win32' ? ';' : ':'),
        'Node must resolve the shared library inventory without adding a conversation package path',
      )
      for (const path of ['.openharness-tmp/tasks', 'node_modules', '.venv', 'venv'])
        assert.equal(
          await access(join(run.execution_directory, path)).then(
            () => true,
            () => false,
          ),
          false,
          `default script execution must not create ${path}`,
        )
      if (packaged) {
        assert(runtimeReport.python, 'packaged Python must execute successfully')
        assert.deepEqual(runtimeReport.python.imports, pythonImports)
        assert.equal(
          await readFile(join(run.execution_directory, 'outputs/science.csv'), 'utf8'),
          'value\n1\n2\n3\n',
        )
        assert.deepEqual(
          (await readFile(join(run.execution_directory, 'outputs/science.png'))).subarray(0, 8),
          Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
        )
        assert.equal(runtimeReport.python.artifact_text, 'MANAGED_PYTHON_OK')
        assert.match(runtimeReport.pnpm, /^\d+\.\d+\.\d+/)
        assert.equal(
          (await readFile(join(run.execution_directory, 'outputs/runtime.docx')))
            .subarray(0, 2)
            .toString(),
          'PK',
        )
        assert.equal(
          (await readFile(join(run.execution_directory, 'outputs/runtime.pdf')))
            .subarray(0, 4)
            .toString(),
          '%PDF',
        )
      }
      if (runtimeReport.python) {
        assert.equal(runtimeReport.python.prefix, runtimeReport.python.base_prefix)
        assert.equal(runtimeReport.python.executable, runtime.python.executable)
        assert(
          runtimeReport.python.pip.includes(runtimeReport.python.base_prefix),
          'pip must come from the shared interpreter',
        )
        if (packaged)
          assertInside(
            run.execution_directory,
            runtimeReport.python.temporary_directory,
            'Python temporary files',
          )
      }
      report.runtime = runtimeReport
      if (process.platform === 'win32') {
        assert.equal(review.execution_mode, 'sandboxed')
        assert.equal(review.sandbox_available, true)
      }

      // The directory button resolves by run ID on the main process, with no path
      // authority delegated to the renderer. Stub only the final OS folder-opening.
      await application.evaluate(({ shell }) => {
        shell.openPath = async (path) => {
          globalThis.__scriptOpenedDirectory = path
          return ''
        }
      })
      await page.getByRole('button', { name: '打开会话文件夹', exact: true }).click()
      await expect
        .poll(() => application.evaluate(() => globalThis.__scriptOpenedDirectory))
        .toBe(await realpath(run.execution_directory))

      await checkpoint('start background script')
      await page.evaluate((id) => window.harness.submitInput(id, 'SCRIPT_BACKGROUND'), run.id)
      await expect
        .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), run.id)).status)
        .toBe('waiting_for_input')
      await expect(page.getByRole('region', { name: '待批准命令' })).toHaveCount(0)
      const running = page.getByRole('region', { name: '运行中的命令' })
      await expect(running).toBeVisible()
      await running.getByRole('button', { name: /查看输出/ }).click()
      const panel = page.getByRole('complementary', { name: '会话文件与命令' })
      await expect(panel).toContainText('BACKGROUND_TICK')
      await expect
        .poll(() =>
          access(join(run.execution_directory, 'outputs/child-started')).then(
            () => true,
            () => false,
          ),
        )
        .toBe(true)
      await checkpoint('stop background script')
      await page.screenshot({ path: join(artifacts, 'script-running-command.png') })
      await panel.getByRole('button', { name: '停止命令', exact: true }).click()
      await expect
        .poll(
          async () =>
            (await page.evaluate((id) => window.harness.getWorkspace(id), run.id)).commands.at(-1)
              .status,
        )
        .toBe('cancelled')
      await expect(running).toHaveCount(0)
      await expect(panel).toContainText('已停止')
      // The child would write this file after five seconds if cancellation only
      // stopped the parent shell. Keep this bounded wait below the app timeout.
      await new Promise((resolve) => setTimeout(resolve, 5500))
      assert.equal(
        await access(join(run.execution_directory, 'outputs/child-escaped')).then(
          () => true,
          () => false,
        ),
        false,
        'stopping a command must also terminate its child processes',
      )
      await checkpoint('revoke session permission and verify subsequent approval')
      await panel.getByRole('button', { name: '撤销会话授权', exact: true }).click()
      await expect
        .poll(
          async () =>
            (await page.evaluate((id) => window.harness.getWorkspace(id), run.id)).command_grants,
        )
        .toEqual([])
      await panel.getByRole('button', { name: '关闭会话文件与命令', exact: true }).click()
      await page.evaluate((id) => window.harness.submitInput(id, 'SCRIPT_AFTER_REVOKE'), run.id)
      const approval = page.getByRole('region', { name: '待批准命令' })
      await expect(approval).toBeVisible()
      await approval.getByRole('button', { name: '拒绝', exact: true }).click()
      await expect
        .poll(
          async () =>
            (await page.evaluate((id) => window.harness.getWorkspace(id), run.id)).commands.at(-1)
              .status,
        )
        .toBe('denied')
      await expect
        .poll(async () => (await page.evaluate((id) => window.harness.getRun(id), run.id)).status)
        .toBe('waiting_for_input')
      await page.reload()
      const restored = await page.evaluate((id) => window.harness.getRun(id), run.id)
      assert.equal(restored.execution_directory, run.execution_directory)
      assert.equal(restored.workspace_path ?? null, null)
      assert.deepEqual(failures, [])
      assert.deepEqual(errors, [])
      report.passed = true
      await checkpoint('script workflow passed')
      console.log(
        JSON.stringify(
          {
            passed: true,
            runId: run.id,
            directory,
            artifacts,
            providerCalls: calls.length,
            packaged: Boolean(packaged),
          },
          null,
          2,
        ),
      )
    },
    180_000,
  )
} catch (error) {
  report.error = error.stack ?? String(error)
  console.error(`[script-workflow] failed during ${currentStage}`)
  if (failures.length) console.error(failures.join('\n'))
  const page = application?.windows()[0]
  if (page) {
    await within(
      'failure screenshot',
      () => page.screenshot({ path: join(artifacts, 'script-failure.png') }),
      5_000,
    ).catch(() => undefined)
    console.error(
      await within('failure window text', () => page.locator('body').innerText(), 5_000).catch(
        () => 'Unable to read test window',
      ),
    )
  }
  throw error
} finally {
  await closeApplication()
  server.closeAllConnections()
  await within(
    'fixture server shutdown',
    () => new Promise((resolve) => server.close(resolve)),
    5_000,
  )
  await saveDiagnostics()
}
