import assert from 'node:assert/strict'
import { spawn, execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import http from 'node:http'
import net from 'node:net'
import { appendFileSync } from 'node:fs'
import { readFile, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'
import { chromium, expect } from '@playwright/test'

const execute = promisify(execFile)
const hostName = 'dev.openharness.chrome'
const registryKey = `HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\${hostName}`

function diagnosticSteps(artifacts) {
  const started = Date.now()
  const deadline = started + 180_000
  const logPath = join(artifacts, 'windows-chrome-progress.log')
  const log = (message) => {
    const line = `[Windows Chrome +${((Date.now() - started) / 1_000).toFixed(1)}s] ${message}\n`
    process.stdout.write(line)
    appendFileSync(logPath, line)
  }
  return async (label, action, timeout = 20_000, cleanup = false) => {
    const remaining = cleanup ? timeout : Math.min(timeout, deadline - Date.now())
    log(`START ${label}`)
    let timer
    try {
      if (remaining <= 0) throw new Error('Windows Chrome verification exceeded 180 seconds')
      const value = await Promise.race([
        Promise.resolve().then(action),
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new Error(`Windows Chrome phase timed out: ${label}`)),
            remaining,
          )
        }),
      ])
      if (!cleanup && Date.now() >= deadline)
        throw new Error('Windows Chrome verification exceeded 180 seconds')
      log(`PASS ${label}`)
      return value
    } catch (error) {
      // Phase names contain no request payloads, credentials or profile contents.
      log(`FAIL ${label}`)
      throw error
    } finally {
      clearTimeout(timer)
    }
  }
}

async function powershell(script, environment = {}) {
  const executable = join(
    process.env.SystemRoot ?? 'C:' + String.fromCharCode(92) + 'Windows',
    'System32',
    'WindowsPowerShell',
    'v1.0',
    'powershell.exe',
  )
  return execute(
    executable,
    [
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-EncodedCommand',
      Buffer.from("$PSModuleAutoLoadingPreference = 'None'\n" + script, 'utf16le').toString(
        'base64',
      ),
    ],
    {
      env: { ...process.env, ...environment },
      windowsHide: true,
      timeout: 10_000,
      maxBuffer: 64 * 1024,
    },
  )
}

async function registration(view) {
  // reg.exe emits the Windows system code page. Use an ASCII Base64 envelope
  // around the Registry API result so Unicode paths are verified faithfully.
  const { stdout } = await powershell(
    `
$ErrorActionPreference = 'Stop'
$view = [Enum]::Parse([Microsoft.Win32.RegistryView], 'Registry' + $env:OPENHARNESS_SMOKE_REGISTRY_VIEW)
$base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
try {
  $key = $base.OpenSubKey('Software\\Google\\Chrome\\NativeMessagingHosts\\dev.openharness.chrome')
  if ($null -eq $key) { [Console]::Write('missing') }
  else {
    try {
      $value = $key.GetValue('')
      if ($value -isnot [string] -or $key.GetValueKind('') -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'Unexpected existing host registration' }
      [Console]::Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value)))
    } finally { $key.Dispose() }
  }
} finally { $base.Dispose() }
`,
    { OPENHARNESS_SMOKE_REGISTRY_VIEW: String(view) },
  )
  const encoded = stdout.trim()
  if (encoded === 'missing') return null
  assert.match(
    encoded,
    /^[A-Za-z0-9+/]+={0,2}$/,
    'the registry check must return an ASCII envelope',
  )
  return Buffer.from(encoded, 'base64').toString('utf8')
}

async function verifyPrivateAcl(directory, configPath) {
  const { stdout } = await powershell(
    `
$ErrorActionPreference = 'Stop'
$user = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$items = [Collections.Generic.List[string]]::new()
foreach ($item in @([IO.DirectoryInfo]::new($env:OPENHARNESS_SMOKE_CHROME_DIRECTORY), [IO.FileInfo]::new($env:OPENHARNESS_SMOKE_CHROME_CONFIG))) {
  $acl = $item.GetAccessControl()
  $allowed = [Collections.Generic.List[string]]::new()
  foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
    if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) {
      $full = $rule.FileSystemRights.HasFlag([Security.AccessControl.FileSystemRights]::FullControl).ToString().ToLowerInvariant()
      $inherited = $rule.IsInherited.ToString().ToLowerInvariant()
      $allowed.Add('{"sid":"' + $rule.IdentityReference.Value + '","fullControl":' + $full + ',"inherited":' + $inherited + '}')
    }
  }
  $directory = ($item -is [IO.DirectoryInfo]).ToString().ToLowerInvariant()
  $protected = $acl.AreAccessRulesProtected.ToString().ToLowerInvariant()
  $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
  $items.Add('{"directory":' + $directory + ',"protected":' + $protected + ',"owner":"' + $owner + '","allowed":[' + [String]::Join(',', $allowed) + ']}')
}
# Values are OS-generated SID strings and booleans, not arbitrary path content.
# Build the small ASCII result without loading ConvertTo-Json's Utility module.
$json = '{"user":"' + $user + '","items":[' + [String]::Join(',', $items) + ']}'
[Console]::Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($json)))
`,
    { OPENHARNESS_SMOKE_CHROME_DIRECTORY: directory, OPENHARNESS_SMOKE_CHROME_CONFIG: configPath },
  )
  const metadata = JSON.parse(Buffer.from(stdout.trim(), 'base64').toString('utf8'))
  assert.equal(metadata.items.length, 2)
  const identities = [metadata.user, 'S-1-5-18'].sort()
  for (const item of metadata.items) {
    assert.deepEqual(
      [...new Set(item.allowed.map((rule) => rule.sid))].sort(),
      identities,
      'only this Windows user and SYSTEM may access the bridge directory and config',
    )
    assert.ok(item.allowed.every((rule) => rule.fullControl))
  }
  const [folder, config] = metadata.items
  assert.equal(folder.directory, true)
  assert.equal(
    folder.protected,
    true,
    'the private directory must not inherit broader parent access',
  )
  assert.equal(folder.owner, metadata.user)
  assert.equal(config.directory, false)
  return { directory: true, config: true, directoryInheritanceDisabled: true }
}

async function waitForExit(child, timeout = 10_000) {
  if (
    (child.exitCode !== null || child.signalCode !== null) &&
    child.stdout?.readableEnded &&
    child.stderr?.readableEnded
  )
    return child.exitCode
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error('The Chrome native host did not exit after its input was closed'))
    }, timeout)
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('close', (code) => {
      clearTimeout(timer)
      resolve(code)
    })
  })
}

async function verifyNativeExecutable({ manifest, extensionId, configPath, pluginRoot, step }) {
  const { ChromeBridge } = await import(pathToFileURL(join(pluginRoot, 'bridge.mjs')).href)
  const { NativeDecoder, nativeFrame } = await import(
    pathToFileURL(join(pluginRoot, 'framing.mjs')).href
  )
  const origin = `chrome-extension://${extensionId}/`
  const host = spawn(manifest.path, [origin, '--parent-window=0'], {
    cwd: dirname(manifest.path),
    windowsHide: true,
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  const bridge = new ChromeBridge({ configPath, timeoutMs: 5_000 })
  const ready = Promise.withResolvers()
  const failure = Promise.withResolvers()
  // The overall deadline can expire before the phase starts awaiting these.
  ready.promise.catch(() => {})
  failure.promise.catch(() => {})
  let closing = false
  let stderr = ''
  const fail = (error) => {
    if (!closing) failure.reject(error)
  }
  const timer = setTimeout(
    () => fail(new Error('Native host did not emit its connected frame')),
    15_000,
  )
  host.on('error', fail)
  host.once('exit', () => fail(new Error(`Native launcher exited before verification: ${stderr}`)))
  host.stdin.on('error', fail)
  host.stderr.on('data', (chunk) => {
    stderr = (stderr + chunk).slice(-8_192)
  })
  const decoder = new NativeDecoder((message) => {
    if (message.event === 'connected') {
      assert.equal(message.protocol_version, 1)
      clearTimeout(timer)
      ready.resolve()
    }
    if (message.method === 'chrome_status') {
      host.stdin.write(
        nativeFrame({ id: message.id, result: { connected: true, native_probe: true } }),
      )
    }
  })
  host.stdout.on('data', (chunk) => {
    try {
      decoder.push(chunk)
    } catch (error) {
      fail(error)
    }
  })
  let failureError
  try {
    // Reject immediately on malformed stdout, spawn failure or premature exit;
    // do not turn a broken native launcher into a polling timeout.
    await step('native executable handshake and pipe round trip', () =>
      Promise.race([
        (async () => {
          await ready.promise
          if (closing) throw new Error('Native executable verification already ended')
          assert.deepEqual(await bridge.request('chrome_status'), {
            connected: true,
            native_probe: true,
          })
        })(),
        failure.promise,
      ]),
    )
  } catch (error) {
    failureError = error
    throw error
  } finally {
    closing = true
    ready.reject(failureError ?? new Error('Native executable verification ended'))
    clearTimeout(timer)
    bridge.close()
    host.stdin.end()
    try {
      await step(
        'native executable EOF and exit',
        async () => {
          assert.equal(await waitForExit(host), 0, 'native host must exit cleanly after EOF')
        },
        12_000,
        true,
      )
    } catch (error) {
      host.kill()
      if (!failureError) throw error
    }
  }
  const rejected = spawn(
    manifest.path,
    ['chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/', '--parent-window=0'],
    {
      cwd: dirname(manifest.path),
      windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  )
  let outputBytes = 0
  rejected.stdout.on('data', (chunk) => {
    outputBytes += chunk.length
  })
  rejected.stderr.resume()
  try {
    await step(
      'reject unregistered native origin',
      async () => {
        assert.notEqual(
          await waitForExit(rejected),
          0,
          'an unregistered extension origin must be rejected',
        )
        assert.equal(outputBytes, 0, 'a rejected origin must receive no native frames')
      },
      12_000,
    )
  } finally {
    if (rejected.exitCode === null && rejected.signalCode === null) rejected.kill()
  }
}

async function verifyMcpExecutable({ pluginRoot, configPath, executable, step }) {
  const manifest = JSON.parse(await readFile(join(pluginRoot, 'mcp.json'), 'utf8'))
  assert.equal(manifest.mcpServers.chrome.command, './launch.exe')
  assert.deepEqual(manifest.mcpServers.chrome.args, ['--mcp'])
  const { LineDecoder } = await import(pathToFileURL(join(pluginRoot, 'framing.mjs')).href)
  const child = spawn(
    join(pluginRoot, 'launch.exe'),
    ['--mcp', '--task-id', 'windows-installed-smoke'],
    {
      cwd: pluginRoot,
      env: {
        ...process.env,
        OPENHARNESS_NODE_BINARY: executable,
        OPENHARNESS_CHROME_CONFIG: configPath,
      },
      windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  )
  let nextId = 0
  let terminalError
  const pending = new Map()
  const fail = (error) => {
    terminalError = error
    for (const request of pending.values()) {
      clearTimeout(request.timer)
      request.reject(error)
    }
    pending.clear()
  }
  const decoder = new LineDecoder((response) => {
    const request = pending.get(response.id)
    if (!request) return
    pending.delete(response.id)
    clearTimeout(request.timer)
    if (response.error) request.reject(new Error(JSON.stringify(response.error)))
    else request.resolve(response.result)
  })
  child.stdout.setEncoding('utf8')
  child.stdout.on('data', (chunk) => {
    try {
      decoder.push(chunk)
    } catch (error) {
      fail(error)
    }
  })
  child.stderr.resume()
  child.stdin.on('error', fail)
  child.on('error', fail)
  child.on('exit', () => fail(new Error('The installed Windows MCP launcher exited')))
  const request = (method, params = {}) =>
    new Promise((resolve, reject) => {
      if (terminalError) {
        reject(terminalError)
        return
      }
      const id = ++nextId
      const timer = setTimeout(() => {
        pending.delete(id)
        reject(new Error(`Installed Windows MCP request timed out: ${method}`))
      }, 10_000)
      pending.set(id, { resolve, reject, timer })
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    })
  let failureError
  try {
    const initialized = await step('installed MCP initialize', () =>
      request('initialize', {
        protocolVersion: '2025-11-25',
        capabilities: {},
        clientInfo: { name: 'windows-packaged-smoke', version: '1' },
      }),
    )
    assert.equal(initialized.serverInfo.name, 'openharness-chrome')
    child.stdin.write(
      `${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`,
    )
    assert.ok(
      (await step('installed MCP tools list', () => request('tools/list'))).tools.some(
        (tool) => tool.name === 'chrome_status',
      ),
    )
    const status = await step('installed MCP Chrome status', () =>
      request('tools/call', { name: 'chrome_status', arguments: {} }),
    )
    assert.notEqual(status.isError, true, JSON.stringify(status.content))
    assert.equal(
      JSON.parse(status.content.find((item) => item.type === 'text').text).connected,
      true,
    )
  } catch (error) {
    failureError = error
    throw error
  } finally {
    fail(new Error('MCP smoke complete'))
    child.stdin.end()
    try {
      await step(
        'installed MCP EOF and exit',
        async () => {
          assert.equal(await waitForExit(child), 0, 'MCP launcher must exit cleanly after EOF')
        },
        12_000,
        true,
      )
    } catch (error) {
      child.kill()
      if (!failureError) throw error
    }
  }
}

async function verifyBrowser({
  extension,
  configPath,
  pluginRoot,
  profile,
  artifacts,
  executable,
  step,
}) {
  const { ChromeBridge } = await import(pathToFileURL(join(pluginRoot, 'bridge.mjs')).href)
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
    response.end(
      '<!doctype html><title>Open Harness Windows Chrome test</title><h1>Background fixture</h1><label>Name <input id="name"></label><button id="go">Save locally</button><p id="result">Waiting</p><script>go.onclick=()=>{result.textContent="Saved: "+document.querySelector("#name").value}</script>',
    )
  })
  const launch = () =>
    chromium.launchPersistentContext(profile, {
      // Playwright's full Chromium distribution is Chrome for Testing. The
      // headless shell cannot load extensions; always select the full executable.
      executablePath: chromium.executablePath(),
      headless: false,
      args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
      timeout: 30_000,
    })
  let context
  let bridge
  let browserPid
  const recordBrowserPid = async () => {
    let session
    try {
      browserPid = await step(
        'record owned Chrome process for cleanup',
        async () => {
          session = await context.browser().newBrowserCDPSession()
          const { processInfo } = await session.send('SystemInfo.getProcessInfo')
          const pid = processInfo.find((item) => item.type === 'browser')?.id
          assert.ok(Number.isSafeInteger(pid) && pid > 0, 'Chrome must report its browser PID')
          return pid
        },
        5_000,
      )
    } catch {
      // A diagnostic fallback is optional; normal browser checks still run.
      // Its phase failure is recorded, and the outer CI deadline owns the tree.
    } finally {
      session?.detach().catch(() => {})
    }
  }
  const worker = async () =>
    context.serviceWorkers()[0] ??
    (await context.waitForEvent('serviceworker', { timeout: 15_000 }))
  let failureError
  try {
    await step(
      'start local browser fixture',
      () =>
        new Promise((resolve, reject) => {
          server.once('error', reject)
          server.listen(0, '127.0.0.1', resolve)
        }),
      5_000,
    )
    const url = `http://127.0.0.1:${server.address().port}/`
    context = await step('launch Chrome for Testing consent profile', launch, 35_000)
    await recordBrowserPid()
    // The CI profile explicitly opts into agent-created tabs. Restart to load
    // this persisted choice through the unmodified production background code.
    const setupWorker = await step('find extension setup worker', worker)
    await step('save CI background tab consent', () =>
      setupWorker.evaluate(() => chrome.storage.local.set({ allowNewTabs: true })),
    )
    await step('close consent profile', () => context.close(), 15_000)
    context = undefined
    browserPid = undefined
    const { socketPath } = JSON.parse(await readFile(configPath, 'utf8'))
    await step(
      'wait for first native pipe to close',
      () =>
        expect
          .poll(
            () =>
              new Promise((resolve, reject) => {
                const probe = net.createConnection(socketPath)
                const timer = setTimeout(() => {
                  probe.destroy()
                  reject(new Error('Native pipe closure probe timed out'))
                }, 2_000)
                probe.once('connect', () => {
                  clearTimeout(timer)
                  probe.destroy()
                  resolve(false)
                })
                probe.once('error', (error) => {
                  clearTimeout(timer)
                  if (['ENOENT', 'ECONNREFUSED'].includes(error.code)) resolve(true)
                  else reject(error)
                })
              }),
            { timeout: 10_000 },
          )
          .toBe(true),
      12_000,
    )
    context = await step('launch Chrome for Testing production extension', launch, 35_000)
    await recordBrowserPid()
    const background = await step('find production extension worker', worker)
    const userPage =
      context.pages()[0] ?? (await step('create private user page', () => context.newPage()))
    await step('load private user fixture', () =>
      userPage.goto(`${url}?user=private`, { timeout: 15_000 }),
    )
    bridge = new ChromeBridge({ configPath, taskId: 'windows-installed-smoke', timeoutMs: 10_000 })
    await step(
      'extension auto-connect over Native Messaging',
      () =>
        expect
          .poll(
            async () => {
              try {
                return (await bridge.request('chrome_status')).allow_new_tabs
              } catch {
                bridge.close()
                return false
              }
            },
            { timeout: 25_000, intervals: [250, 500, 1_000] },
          )
          .toBe(true),
      30_000,
    )
    await verifyMcpExecutable({ pluginRoot, configPath, executable, step })
    const rpc = (method, parameters = {}) =>
      step(`browser RPC ${method}`, () => bridge.request(method, parameters), 12_000)
    const activeBefore = await step('read active user tab', () =>
      background.evaluate(async () => (await chrome.tabs.query({ active: true }))[0].id),
    )
    assert.equal(
      (await rpc('chrome_tabs')).tabs.length,
      0,
      'unshared user tabs must remain private',
    )
    const first = await rpc('chrome_open', { url: `${url}?agent=first` })
    assert.notEqual(first.tab_id, activeBefore)
    assert.ok(Number.isInteger(first.group_id) && first.group_id >= 0)
    assert.match(
      (
        await step('read task tab group', () =>
          background.evaluate((id) => chrome.tabGroups.get(id), first.group_id),
        )
      ).title,
      /Open Harness/,
    )
    let snapshot
    await step(
      'wait for background fixture snapshot',
      () =>
        expect
          .poll(
            async () => {
              snapshot = await bridge.request('chrome_snapshot')
              return snapshot.text
            },
            { timeout: 15_000 },
          )
          .toContain('Background fixture'),
      20_000,
    )
    const input = snapshot.elements.find((element) => element.role === 'input')
    assert.ok(input)
    snapshot = await rpc('chrome_action', {
      revision: snapshot.revision,
      action: 'fill',
      ref: input.ref,
      text: 'WINDOWS-4827',
    })
    assert.equal(snapshot.dispatched, true)
    const button = snapshot.elements.find((element) => element.name === 'Save locally')
    assert.ok(button)
    assert.equal(
      (
        await rpc('chrome_action', {
          revision: snapshot.revision,
          action: 'click',
          ref: button.ref,
        })
      ).dispatched,
      true,
    )
    assert.match((await rpc('chrome_snapshot')).text, /Saved: WINDOWS-4827/)
    const screenshot = await rpc('chrome_screenshot')
    assert.equal(screenshot.image.mimeType, 'image/jpeg')
    assert.ok(screenshot.image.data.length > 1_000)
    await writeFile(
      join(artifacts, 'windows-chrome-background.jpg'),
      Buffer.from(screenshot.image.data, 'base64'),
    )
    const second = await rpc('chrome_open', { url: `${url}?agent=second` })
    assert.equal(second.group_id, first.group_id, 'the same task must reuse its Chrome group')
    assert.equal((await rpc('chrome_release', { close_owned: true })).closed, true)
    await rpc('chrome_select_tab', { tab_id: first.tab_id, origin: new URL(url).origin })
    assert.equal((await rpc('chrome_release', { close_owned: true })).closed, true)
    const remaining = await step('read remaining tabs', () =>
      background.evaluate(async () => (await chrome.tabs.query({})).map((tab) => tab.id)),
    )
    assert.ok(!remaining.includes(first.tab_id) && !remaining.includes(second.tab_id))
    assert.ok(remaining.includes(activeBefore))
    assert.equal(
      await step('check private user input unchanged', () =>
        userPage.locator('#name').inputValue({ timeout: 5_000 }),
      ),
      '',
    )
    assert.equal(
      await step('check active user tab unchanged', () =>
        background.evaluate(async () => (await chrome.tabs.query({ active: true }))[0].id),
      ),
      activeBefore,
    )
    assert.equal(
      await step('check user tab still exists', () =>
        background.evaluate(async (id) => (await chrome.tabs.get(id)).id, activeBefore),
      ),
      activeBefore,
    )
    return {
      browser: await context.browser().version(),
      checks: [
        'installed native host',
        'real extension Native Messaging',
        'installed Windows MCP launcher',
        'authenticated named pipe',
        'private user tab',
        'background task group',
        'DOM snapshot',
        'fill and click',
        'background screenshot',
        'group reuse',
        'close only owned tabs',
        'active user tab unchanged',
      ],
    }
  } catch (error) {
    failureError = error
    throw error
  } finally {
    bridge?.close()
    let cleanupError
    try {
      if (context) await step('close Chrome for Testing', () => context.close(), 15_000, true)
    } catch (error) {
      cleanupError = error
      if (browserPid) {
        try {
          await step(
            'terminate owned Chrome process after close failure',
            () =>
              execute('taskkill.exe', ['/PID', String(browserPid), '/T', '/F'], {
                windowsHide: true,
                timeout: 10_000,
              }),
            12_000,
            true,
          )
        } catch {
          // Preserve the close failure; the failed termination is in the log.
        }
      }
    }
    try {
      await step(
        'close local browser fixture',
        () =>
          new Promise((resolve, reject) => {
            server.close((error) =>
              error && error.code !== 'ERR_SERVER_NOT_RUNNING' ? reject(error) : resolve(),
            )
            server.closeAllConnections()
          }),
        5_000,
        true,
      )
    } catch (error) {
      cleanupError ??= error
    }
    if (!failureError && cleanupError) throw cleanupError
  }
}

/** Windows-only installed-app verification. Never run against a real user profile. */
export async function verifyWindowsChrome({ page, runtime, directory, artifacts }) {
  assert.equal(process.platform, 'win32')
  assert.equal(runtime.platform, 'win32')
  assert.equal(
    process.env.GITHUB_ACTIONS,
    'true',
    'registry smoke runs only in the disposable CI Windows account',
  )
  const step = diagnosticSteps(artifacts)
  for (const view of [32, 64]) {
    assert.equal(
      await step(`check initial HKCU ${view}-bit registration`, () => registration(view)),
      null,
      'refuse to replace a pre-existing Chrome host registration',
    )
  }
  const manifestPath = join(directory, 'chrome-use', `${hostName}.json`)
  const pluginRoot = join(runtime.resources, 'plugins', 'chrome-use')
  let failureError
  try {
    const chromePlugin = (
      await step('list installed bundled plugins', () =>
        page.evaluate(() => window.harness.bundledPlugins()),
      )
    ).find((plugin) => plugin.id === 'chrome-use')
    assert.equal(chromePlugin.available, true, 'the installed Windows app must expose Chrome setup')
    await step(
      'prepare Chrome through installed app IPC',
      () => page.evaluate(() => window.harness.connectChrome('repair')),
      30_000,
    )
    for (const view of [32, 64])
      assert.equal(
        await step(`read prepared HKCU ${view}-bit registration`, () => registration(view)),
        manifestPath,
      )
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
    assert.equal(manifest.name, hostName)
    assert.equal(manifest.type, 'stdio')
    assert.match(manifest.path, /native-host-[a-f0-9]{16}\.exe$/i)
    const extension = join(directory, 'chrome-use', 'extension')
    const extensionManifest = JSON.parse(await readFile(join(extension, 'manifest.json'), 'utf8'))
    const extensionId = createHash('sha256')
      .update(Buffer.from(extensionManifest.key, 'base64'))
      .digest('hex')
      .slice(0, 32)
      .replace(/[0-9a-f]/g, (hex) => String.fromCharCode(97 + parseInt(hex, 16)))
    assert.deepEqual(manifest.allowed_origins, [`chrome-extension://${extensionId}/`])
    const configPath = join(directory, 'chrome-use', 'config.json')
    const acl = await step('verify private Windows directory and config ACL', () =>
      verifyPrivateAcl(dirname(configPath), configPath),
    )
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    const installedBeforeRepair = await step('read plugin settings before repeat repair', () =>
      page.evaluate(async () => {
        const settings = await window.harness.extensionSettings()
        return {
          revision: settings.config.revision,
          plugin: settings.plugins.find((plugin) => plugin.id === 'chrome-use'),
        }
      }),
    )
    assert.equal(typeof installedBeforeRepair.revision, 'number')
    assert.equal(installedBeforeRepair.plugin?.enabled, true)
    await step(
      'repeat Chrome repair through installed app IPC',
      () => page.evaluate(() => window.harness.connectChrome('repair')),
      30_000,
    )
    const installedAfterRepair = await step('read plugin settings after repeat repair', () =>
      page.evaluate(async () => {
        const settings = await window.harness.extensionSettings()
        return {
          revision: settings.config.revision,
          plugin: settings.plugins.find((plugin) => plugin.id === 'chrome-use'),
        }
      }),
    )
    assert.deepEqual(
      installedAfterRepair,
      installedBeforeRepair,
      'repair must recognize the canonical Windows plugin path without reinstalling it',
    )
    const repairedConfig = JSON.parse(await readFile(configPath, 'utf8'))
    // Do not include either token in an assertion diagnostic or saved report.
    assert.ok(repairedConfig.token === config.token, 'repair must preserve the existing token')
    assert.equal(repairedConfig.socketPath, config.socketPath)
    for (const view of [32, 64])
      assert.equal(
        await step(`read repaired HKCU ${view}-bit registration`, () => registration(view)),
        manifestPath,
      )
    const pipeParts = config.socketPath.split(String.fromCharCode(92))
    assert.deepEqual(pipeParts.slice(0, 4), ['', '', '.', 'pipe'])
    assert.equal(pipeParts.length, 5)
    assert.match(pipeParts[4], /^openharness-chrome-[a-f0-9]{32}$/)
    assert.equal(
      (
        await step('read installed app connection status', () =>
          page.evaluate(() => window.harness.chromeConnectionStatus()),
        )
      ).configured,
      true,
    )
    await verifyNativeExecutable({ manifest, extensionId, configPath, pluginRoot, step })
    const browser = await verifyBrowser({
      extension,
      configPath,
      pluginRoot,
      profile: join(directory, 'test-chrome-profile'),
      artifacts,
      executable: runtime.executable,
      step,
    })
    return { ...browser, acl, repeatRepairPreservedRegistration: true }
  } catch (error) {
    failureError = error
    throw error
  } finally {
    let cleanupError
    for (const view of [32, 64]) {
      try {
        await step(
          `clean up HKCU ${view}-bit registration`,
          async () => {
            const value = await registration(view)
            if (value === manifestPath) {
              await execute('reg.exe', ['delete', registryKey, '/f', `/reg:${view}`], {
                windowsHide: true,
                timeout: 5_000,
              })
            } else {
              assert.equal(
                value,
                null,
                'refuse to remove a registration changed by another process',
              )
            }
          },
          17_000,
          true,
        )
      } catch (error) {
        cleanupError ??= error
      }
    }
    if (!failureError && cleanupError) throw cleanupError
  }
}
