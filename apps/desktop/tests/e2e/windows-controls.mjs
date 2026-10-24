import assert from 'node:assert/strict'
import { spawn, execFile } from 'node:child_process'
import { appendFileSync } from 'node:fs'
import { readFile, realpath, writeFile } from 'node:fs/promises'
import http from 'node:http'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'

const execute = promisify(execFile)
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
function alive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if (error.code === 'ESRCH') return false
    throw error
  }
}
async function until(check, description, timeout = 10_000) {
  const end = Date.now() + timeout
  while (Date.now() < end) {
    const result = await check()
    if (result) return result
    await pause(100)
  }
  throw new Error(`Windows controls timed out: ${description}`)
}

// Real installed native/MCP processes. Every request and process exit is bounded.
function protocol(command, args, environment, jsonrpc = true) {
  const child = spawn(command, args, {
    env: environment,
    windowsHide: true,
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  let sequence = 0,
    buffer = '',
    terminalError
  const pending = new Map()
  const exited = Promise.withResolvers()
  const fail = (error) => {
    terminalError = error
    for (const { reject, timer } of pending.values()) {
      clearTimeout(timer)
      reject(error)
    }
    pending.clear()
  }
  child.stderr.resume()
  child.stdin.on('error', fail)
  child.on('error', fail)
  child.once('close', (code) => {
    fail(new Error(`Controls process closed (${code})`))
    exited.resolve(code)
  })
  child.stdout.setEncoding('utf8')
  child.stdout.on('data', (chunk) => {
    buffer += chunk
    if (buffer.length > 8 * 1024 * 1024) {
      fail(new Error('Controls response exceeded 8 MiB'))
      child.kill()
      return
    }
    let newline
    while ((newline = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, newline)
      buffer = buffer.slice(newline + 1)
      try {
        const message = JSON.parse(line),
          request = pending.get(message.id)
        assert.ok(request, 'native/MCP response must match an outstanding request')
        clearTimeout(request.timer)
        pending.delete(message.id)
        if (message.error) request.reject(new Error(JSON.stringify(message.error)))
        else request.resolve(message.result)
      } catch (error) {
        fail(error)
        child.kill()
        return
      }
    }
  })
  const request = (method, params = {}) =>
    new Promise((resolve, reject) => {
      if (terminalError) {
        reject(terminalError)
        return
      }
      const id = ++sequence
      const timer = setTimeout(() => {
        pending.delete(id)
        reject(new Error(`Controls request timed out: ${method}`))
        child.kill()
      }, 20_000)
      pending.set(id, { resolve, reject, timer })
      child.stdin.write(
        `${JSON.stringify({ ...(jsonrpc ? { jsonrpc: '2.0' } : {}), id, method, params })}\n`,
      )
    })
  return {
    child,
    request,
    async initialize(name) {
      const result = await request('initialize', {
        protocolVersion: '2025-11-25',
        capabilities: {},
        clientInfo: { name: 'windows-controls-smoke', version: '1' },
      })
      assert.equal(result.serverInfo.name, name)
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`,
      )
    },
    async tool(name, args = {}) {
      const result = await request('tools/call', { name, arguments: args })
      const metadata = JSON.parse(result.content.find((item) => item.type === 'text').text)
      if (result.isError)
        throw new Error(metadata.error ?? metadata.message ?? JSON.stringify(metadata))
      return { ...metadata, image: result.content.find((item) => item.type === 'image') }
    },
    async close() {
      if (child.exitCode !== null || child.signalCode !== null) {
        assert.equal(child.exitCode, 0, 'installed control process must not exit with an error')
        return
      }
      child.stdin.end()
      const timer = setTimeout(() => child.kill(), 8_000)
      try {
        assert.equal(
          await exited.promise,
          0,
          'installed control process must exit cleanly after EOF',
        )
      } finally {
        clearTimeout(timer)
      }
    },
  }
}

/** Exercises only CI-owned fixture windows; never inspects a user's desktop. */
export async function verifyWindowsControls({ page, runtime, directory, artifacts }) {
  assert.equal(process.platform, 'win32')
  assert.equal(process.env.GITHUB_ACTIONS, 'true')
  const deadline = Date.now() + 120_000
  const checks = [],
    clients = [],
    fixtures = [],
    helperPids = new Set()
  const report = { passed: false, checks }
  const log = (label) => {
    const line = `[Windows controls] ${label}\n`
    process.stdout.write(line)
    appendFileSync(join(artifacts, 'windows-controls-progress.log'), line)
  }
  const step = async (label, action, timeout = 20_000, cleanup = false) => {
    log(`START ${label}`)
    let timer
    try {
      const remaining = cleanup ? timeout : Math.min(timeout, deadline - Date.now())
      if (remaining <= 0) throw new Error('Windows controls exceeded 120 seconds')
      const value = await Promise.race([
        Promise.resolve().then(action),
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new Error(`Windows controls phase timed out: ${label}`)),
            remaining,
          )
        }),
      ])
      if (!cleanup && Date.now() >= deadline)
        throw new Error('Windows controls exceeded 120 seconds')
      log(`PASS ${label}`)
      return value
    } catch (error) {
      log(`FAIL ${label}`)
      throw error
    } finally {
      clearTimeout(timer)
    }
  }
  const root = join(runtime.resources, 'plugins')
  const helper = join(root, 'computer-use', 'windows', 'bin', 'OpenHarnessComputerUse.exe')
  const environment = {
    ...process.env,
    OPENHARNESS_NODE_BINARY: runtime.executable,
    OPENHARNESS_COMPUTER_APP: '',
    OPENHARNESS_COMPUTER_HELPER: helper,
    OPENHARNESS_BROWSER_RUNTIME: join(runtime.resources, 'browser-runtime', 'index.js'),
  }
  delete environment.PLAYWRIGHT_BROWSERS_PATH // use the CI-installed browser cache
  const mcp = async (id, name) => {
    const plugin = join(root, id)
    const manifest = JSON.parse(await readFile(join(plugin, 'mcp.json'), 'utf8'))
    const service = Object.values(manifest.mcpServers)[0]
    assert.equal(service.command, './launch.exe')
    assert.deepEqual(service.args, ['--mcp'])
    const client = protocol(
      join(plugin, 'launch.exe'),
      ['--mcp', '--task-id', 'windows-controls-smoke'],
      environment,
    )
    clients.push(client)
    await step(`${id} installed MCP initialize`, () => client.initialize(name))
    return client
  }
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
    response.end(
      '<!doctype html><h1>Isolated Windows browser</h1><label>Name <input id="name"></label><button id="save">Save locally</button><p id="result">Waiting</p><script>save.onclick=()=>result.textContent="Saved: "+document.querySelector("#name").value</script>',
    )
  })
  let originalError
  try {
    const available = await step('installed desktop capability entry', () =>
      page.evaluate(() => window.harness.bundledPlugins()),
    )
    const computer = available.find((item) => item.id === 'computer-use')
    assert.equal(computer.available, true)
    assert.equal(
      computer.desktop?.available,
      true,
      computer.desktop?.error ?? 'An interactive Windows desktop is required',
    )
    assert.equal(
      computer.permissions,
      undefined,
      'Windows must not pretend to require macOS grants',
    )
    checks.push('installed Windows desktop readiness UI')

    await step(
      'local isolated-browser fixture',
      () =>
        new Promise((resolve, reject) => {
          server.once('error', reject)
          server.listen(0, '127.0.0.1', resolve)
        }),
    )
    const browser = await mcp('browser-use', 'openharness-browser')
    let browserState = (
      await step(
        'isolated browser open',
        () => browser.tool('browser_open', { url: `http://127.0.0.1:${server.address().port}/` }),
        30_000,
      )
    ).snapshot
    assert.ok(browserState, 'browser_open must return its real page snapshot')
    assert.match(browserState.text, /Isolated Windows browser/)
    const browserInput = browserState.elements.find((item) => item.role === 'input')
    assert.ok(browserInput)
    browserState = (
      await step('isolated browser fill', () =>
        browser.tool('browser_action', {
          revision: browserState.revision,
          action: 'fill',
          ref: browserInput.ref,
          text: '独立浏览器 Windows',
        }),
      )
    ).snapshot
    assert.ok(browserState, 'browser fill must return its real page snapshot')
    const browserButton = browserState.elements.find((item) => item.name === 'Save locally')
    assert.ok(browserButton)
    browserState = (
      await step('isolated browser click', () =>
        browser.tool('browser_action', {
          revision: browserState.revision,
          action: 'click',
          ref: browserButton.ref,
        }),
      )
    ).snapshot
    assert.ok(browserState, 'browser click must return its real page snapshot')
    assert.match(browserState.text, /Saved: 独立浏览器 Windows/)
    assert.equal(
      (await step('isolated browser close', () => browser.tool('browser_close'))).closed,
      true,
    )
    await step('isolated browser EOF', () => browser.close())
    checks.push('installed independent browser launcher and actual DOM actions')

    const build = fileURLToPath(
      new URL('../../../../plugins/computer-use/windows/build.mjs', import.meta.url),
    )
    await step(
      'build independent WinForms fixture',
      () => execute(process.execPath, [build, '--fixture'], { windowsHide: true, timeout: 45_000 }),
      50_000,
    )
    const fixturePath = fileURLToPath(
      new URL(
        '../../../../plugins/computer-use/windows/bin/ComputerUseFixture.exe',
        import.meta.url,
      ),
    )
    const stateFile = join(directory, 'controls-state.json'),
      otherFile = join(directory, 'controls-other.json'),
      passwordFile = join(directory, 'controls-password.json')
    const readState = async (file) => {
      try {
        return JSON.parse(await readFile(file, 'utf8'))
      } catch (error) {
        if (error.code === 'ENOENT' || error instanceof SyntaxError) return undefined
        throw error
      }
    }
    const startFixture = async (file, extra = []) => {
      const child = spawn(fixturePath, ['--medium-integrity', '--state', file, ...extra], {
        windowsHide: false,
        stdio: 'ignore',
      })
      fixtures.push(child)
      let launchError
      child.once('error', (error) => {
        launchError = error
      })
      return step('WinForms fixture ready', () =>
        until(async () => {
          if (launchError) throw launchError
          const state = await readState(file)
          if (state?.ready) {
            assert.equal(state.pid, child.pid)
            return state
          }
          if (child.exitCode !== null)
            throw new Error(`WinForms fixture exited (${child.exitCode})`)
        }, 'fixture ready'),
      )
    }
    const first = await startFixture(stateFile)
    const other = await startFixture(otherFile)
    const computerClient = await mcp('computer-use', 'openharness-computer-use')
    const tool = (name, args) => step(name, () => computerClient.tool(name, args))
    const status = await tool('computer_status')
    assert.ok(Number.isSafeInteger(status.helper_pid) && status.helper_pid > 0)
    helperPids.add(status.helper_pid)
    assert.equal(await realpath(status.helper_path), await realpath(helper))
    assert.equal(status.platform, 'windows')
    assert.equal(status.desktop_available, true)
    assert.equal(status.permissions_requested, false)
    assert.equal(status.capabilities.foreground_input, true)
    assert.equal(status.capabilities.background_input, false)
    assert.equal(status.accessibility, undefined)
    const apps = await tool('computer_apps')
    const firstApp = apps.applications.find((item) => item.pid === first.pid)
    assert.match(firstApp.bundle_id, /^win32:[a-f0-9]{64}$/)
    await step('reject incorrect executable identity', () =>
      assert.rejects(
        computerClient.tool('computer_select_app', {
          bundle_id: `win32:${'0'.repeat(64)}`,
          pid: first.pid,
        }),
        /APP_NOT_FOUND|IDENTITY/,
      ),
    )
    await tool('computer_select_app', { bundle_id: firstApp.bundle_id, pid: first.pid })
    let observation = await tool('computer_observe')
    const ref = (snapshot, id) => {
      const names = {
        TaskInput: 'Task input',
        ApplyButton: 'Apply',
        PasswordInput: 'Password',
        ScrollPanel: 'Scroll area',
      }
      const item = snapshot.elements.find(
        (item) => item.automation_id === id || item.name === names[id],
      )
      assert.ok(item, `UIA element ${id}`)
      return item
    }
    await tool('computer_click', {
      snapshot_id: observation.snapshot_id,
      ref: ref(observation, 'TaskInput').ref,
    })
    observation = await tool('computer_observe')
    const consumed = observation.snapshot_id
    const text = 'Open Harness 中文🙂'
    await tool('computer_type', { snapshot_id: consumed, text })
    await step('verify independent Unicode input outcome', () =>
      until(async () => (await readState(stateFile))?.text === text, 'Unicode input'),
    )
    await step('consumed snapshot rejected', () =>
      assert.rejects(
        computerClient.tool('computer_key', { snapshot_id: consumed, key: 'enter' }),
        /STALE_SNAPSHOT/,
      ),
    )
    observation = await tool('computer_observe')
    await tool('computer_key', { snapshot_id: observation.snapshot_id, key: 'end' })
    observation = await tool('computer_observe')
    await tool('computer_type', { snapshot_id: observation.snapshot_id, text: 'X' })
    await step('verify appended character', () =>
      until(async () => (await readState(stateFile))?.text === `${text}X`, 'appended character'),
    )
    observation = await tool('computer_observe')
    await tool('computer_key', { snapshot_id: observation.snapshot_id, key: 'backspace' })
    await step('verify native editing key outcome', () =>
      until(async () => (await readState(stateFile))?.text === text, 'backspace'),
    )
    observation = await tool('computer_observe')
    await tool('computer_click', {
      snapshot_id: observation.snapshot_id,
      ref: ref(observation, 'ApplyButton').ref,
    })
    await step('verify independent native click outcome', () =>
      until(async () => (await readState(stateFile))?.clicks === 1, 'UIA invoke'),
    )
    observation = await tool('computer_observe', { screenshot: true })
    assert.equal(observation.image.mimeType, 'image/png')
    const png = Buffer.from(observation.image.data, 'base64')
    assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a')
    await writeFile(join(artifacts, 'windows-computer-fixture.png'), png)
    assert.match(observation.coordinate_space, /physical pixels/)
    const button = ref(observation, 'ApplyButton').bounds
    await tool('computer_click', {
      snapshot_id: observation.snapshot_id,
      x: button.x + button.width / 2,
      y: button.y + button.height / 2,
    })
    await step('verify coordinate click outcome', () =>
      until(async () => (await readState(stateFile))?.clicks === 2, 'coordinate click'),
    )
    observation = await tool('computer_observe', { screenshot: true })
    const panel = ref(observation, 'ScrollPanel').bounds
    const beforeScroll = (await readState(stateFile)).scroll_y
    assert.equal(typeof beforeScroll, 'number')
    await tool('computer_scroll', {
      snapshot_id: observation.snapshot_id,
      x: panel.x + panel.width / 2,
      y: panel.y + Math.min(50, panel.height / 2),
      delta_y: -240,
    })
    await step('verify independent native scroll outcome', () =>
      until(async () => {
        const state = await readState(stateFile)
        return typeof state?.scroll_y === 'number' && state.scroll_y !== beforeScroll
      }, 'scroll offset'),
    )
    checks.push(
      'actual UIA observation and executable identity',
      'Unicode typing',
      'reference and coordinate clicks',
      'native navigation keys',
      'window-only PNG',
      'scroll outcome',
      'single-use snapshot rejection',
    )

    observation = await tool('computer_observe')
    const secondary = await mcp('computer-use', 'openharness-computer-use')
    const secondaryStatus = await step('secondary helper identity', () =>
      secondary.tool('computer_status'),
    )
    assert.ok(Number.isSafeInteger(secondaryStatus.helper_pid) && secondaryStatus.helper_pid > 0)
    helperPids.add(secondaryStatus.helper_pid)
    await step('select separate private fixture', () =>
      secondary.tool('computer_select_app', { bundle_id: firstApp.bundle_id, pid: other.pid }),
    )
    await step('reject input after foreground moves', () =>
      assert.rejects(
        computerClient.tool('computer_type', {
          snapshot_id: observation.snapshot_id,
          text: 'must not escape',
        }),
        /FOCUS_CHANGED/,
      ),
    )
    assert.equal((await readState(stateFile)).text, text)
    assert.equal((await readState(otherFile)).text, '')
    assert.equal((await readState(otherFile)).clicks, 0)
    await step('release secondary helper', () => secondary.tool('computer_release'))
    await step('secondary helper exits on release', () =>
      until(() => !alive(secondaryStatus.helper_pid), 'secondary helper exit'),
    )
    await step('secondary helper EOF', () => secondary.close())
    checks.push('separate user window untouched and focus loss rejected')

    const password = await startFixture(passwordFile, ['--password-focus'])
    await tool('computer_select_app', { bundle_id: firstApp.bundle_id, pid: password.pid })
    observation = await tool('computer_observe')
    const protectedField = ref(observation, 'PasswordInput')
    assert.equal(protectedField.is_password, true)
    assert.equal(protectedField.value, undefined)
    await step('password input refused', () =>
      assert.rejects(
        computerClient.tool('computer_type', {
          snapshot_id: observation.snapshot_id,
          text: 'must not enter',
        }),
        /SECURE_FIELD|PASSWORD_FIELD|SECURE_INPUT/,
      ),
    )
    assert.equal((await readState(passwordFile)).password_length, 0)
    checks.push('password refusal and value redaction')

    await tool('computer_release')
    await step('released helper process exits', () =>
      until(() => !alive(status.helper_pid), 'released helper exit'),
    )
    await step('release clears selection', () =>
      assert.rejects(computerClient.tool('computer_observe'), /APP_NOT_SELECTED/),
    )
    const restarted = await tool('computer_status')
    helperPids.add(restarted.helper_pid)
    await step('MCP EOF cleanup', () => computerClient.close())
    await step('helper exits with MCP EOF', () =>
      until(() => !alive(restarted.helper_pid), 'MCP EOF helper exit'),
    )
    const direct = protocol(helper, [], environment, false)
    clients.push(direct)
    const nativeStatus = await step('native helper stdio identity', () => direct.request('status'))
    helperPids.add(nativeStatus.helper_pid)
    assert.equal(nativeStatus.helper_pid, direct.child.pid)
    await step('direct native EOF exit', () => direct.close())
    checks.push('release and real helper EOF process cleanup')
    report.fixture = await readState(stateFile)
    report.otherFixture = await readState(otherFile)
    report.passed = true
  } catch (error) {
    originalError = error
    report.error = error instanceof Error ? error.stack : String(error)
    throw error
  } finally {
    let cleanupError
    for (const client of clients) {
      try {
        await step('close test MCP/native process', () => client.close(), 12_000, true)
      } catch (error) {
        cleanupError ??= error
        client.child.kill()
      }
    }
    for (const pid of [...fixtures.map((child) => child.pid), ...helperPids]) {
      if (!Number.isSafeInteger(pid)) continue
      try {
        if (alive(pid))
          await execute('taskkill.exe', ['/PID', String(pid), '/T', '/F'], {
            windowsHide: true,
            timeout: 5_000,
          })
      } catch (error) {
        if (alive(pid)) cleanupError ??= error
      }
    }
    await step(
      'close local controls fixture',
      () =>
        new Promise((resolve, reject) => {
          server.close((error) =>
            error && error.code !== 'ERR_SERVER_NOT_RUNNING' ? reject(error) : resolve(),
          )
          server.closeAllConnections()
        }),
      5_000,
      true,
    ).catch((error) => {
      cleanupError ??= error
    })
    if (cleanupError) {
      report.passed = false
      report.cleanupError = String(cleanupError)
    }
    await writeFile(
      join(artifacts, 'windows-controls-report.json'),
      JSON.stringify(report, null, 2),
    )
    if (!originalError && cleanupError) throw cleanupError
  }
  return report
}
