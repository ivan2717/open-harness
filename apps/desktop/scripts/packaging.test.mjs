import assert from 'node:assert/strict'
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'vitest'
import { directoryBuildArguments } from './package-dir.mjs'
import { verifyMacSignature } from './verify-signature.mjs'
import { stagePluginResources } from './plugin-resources.mjs'
import { backendBuildPlan } from './backend.mjs'

test('Windows backend builds ship the sandbox helper next to the API', async () => {
  for (const release of [false, true]) {
    const plan = backendBuildPlan({ platform: 'win32', arch: 'x64', release })
    assert.ok(plan.arguments.includes('agent-local'))
    assert.ok(plan.filenames.includes('openharness-sandbox.exe'))
    assert.equal(plan.windowsTarget, release ? 'x86_64-pc-windows-msvc' : undefined)
    if (release) assert.ok(plan.filenames.includes('api.exe'))
  }
  const config = await readFile(new URL('../electron-builder.yml', import.meta.url), 'utf8')
  assert.match(config, /- from: resources\/bin\s+to: bin/)
})

test('Chrome-only preparation and other platforms do not build the Windows sandbox helper', () => {
  const chrome = backendBuildPlan({
    platform: 'win32',
    arch: 'x64',
    release: true,
    chromeHostOnly: true,
  })
  assert.deepEqual(chrome.filenames, ['openharness-chrome-host.exe'])
  assert.ok(!chrome.arguments.includes('agent-local'))
  for (const platform of ['darwin', 'linux']) {
    const plan = backendBuildPlan({ platform, arch: 'arm64', release: true })
    assert.deepEqual(plan.filenames, ['api'])
    assert.ok(!plan.arguments.includes('agent-local'))
  }
})

const context = {
  electronPlatformName: 'darwin',
  appOutDir: '/build/mac-arm64',
  packager: {
    appInfo: { productFilename: 'Open Harness', macBundleIdentifier: 'dev.openharness.desktop' },
  },
}

test('local macOS directory builds use a persistent signer in a staging directory', async () => {
  assert.deepEqual(directoryBuildArguments('darwin'), [
    '--dir',
    '-c.mac.identity=-',
    '-c.mac.sign=./scripts/sign-local-mac.mjs',
    '-c.directories.output=release-next',
  ])
  assert.deepEqual(directoryBuildArguments('linux'), ['--dir'])
  assert.deepEqual(directoryBuildArguments('win32'), ['--dir'])
  const { scripts } = JSON.parse(
    await readFile(new URL('../package.json', import.meta.url), 'utf8'),
  )
  assert.ok(scripts['package:dir'].endsWith('node scripts/package-dir.mjs'))
  assert.ok(scripts.package.endsWith('&& electron-builder'))
  const config = await readFile(new URL('../electron-builder.yml', import.meta.url), 'utf8')
  assert.match(config, /^afterSign: \.\/scripts\/verify-signature\.mjs$/m)
  assert.match(config, /^mac:\n  forceCodeSigning: true$/m)
  assert.doesNotMatch(config, /identity:/)
})

test('local packaging rejects ad-hoc or mismatched helper certificate identities', () => {
  const local = {
    ...context,
    packager: {
      ...context.packager,
      platformSpecificBuildOptions: { sign: './scripts/sign-local-mac.mjs' },
    },
  }
  for (const helper of ['cdhash H"abc"', `certificate leaf = H"${'2'.repeat(40)}"`]) {
    assert.throws(
      () =>
        verifyMacSignature(local, (_command, args) => ({
          status: 0,
          stderr: args.includes('-r-')
            ? `designated => identifier "test" and ${args.at(-1).endsWith('Open Harness Computer Use.app') ? helper : `certificate leaf = H"${'1'.repeat(40)}"`}`
            : `Identifier=${args.at(-1).endsWith('Open Harness Computer Use.app') ? 'dev.openharness.computeruse' : 'dev.openharness.desktop'}\n`,
        })),
      /stable certificate-backed/,
    )
  }
})

test('the post-sign hook verifies nested code and the actual bundle identity', () => {
  const calls = []
  verifyMacSignature(context, (command, args, options) => {
    calls.push({ command, args, options })
    return {
      status: 0,
      stdout: '',
      stderr: `Identifier=${args.at(-1).endsWith('Open Harness Computer Use.app') ? 'dev.openharness.computeruse' : 'dev.openharness.desktop'}\n`,
    }
  })
  assert.equal(calls.length, 4)
  assert.equal(calls[0].command, '/usr/bin/codesign')
  assert.deepEqual(calls[0].args, [
    '--verify',
    '--deep',
    '--strict',
    '--verbose=2',
    '/build/mac-arm64/Open Harness.app',
  ])
  assert.deepEqual(calls[1].args, ['--display', '--verbose=2', '/build/mac-arm64/Open Harness.app'])
  assert.equal(calls[0].options.timeout, 120_000)
  assert.deepEqual(calls[2].args, [
    '--verify',
    '--strict',
    '--verbose=2',
    '/build/mac-arm64/Open Harness.app/Contents/Resources/plugins/computer-use/bin/Open Harness Computer Use.app',
  ])
  assert.equal(calls[3].args[0], '--display')
})

test('the independent Computer Use app must have its own signing identity', () => {
  assert.throws(
    () =>
      verifyMacSignature(context, () => ({
        status: 0,
        stderr: 'Identifier=dev.openharness.desktop\n',
      })),
    /Computer Use signature identity mismatch/,
  )
})

test('invalid or missing nested signatures fail packaging', () => {
  assert.throws(
    () => verifyMacSignature(context, () => ({ status: 1, stderr: 'invalid nested code' })),
    /invalid nested code/,
  )
})

test('an upstream Electron signature cannot pass the identity check', () => {
  assert.throws(
    () => verifyMacSignature(context, () => ({ status: 0, stderr: 'Identifier=Electron\n' })),
    /Signature identity mismatch/,
  )
})

test('verification errors propagate and non-macOS builds do not invoke codesign', () => {
  const error = new Error('codesign timed out')
  assert.throws(() => verifyMacSignature(context, () => ({ error })), error)
  verifyMacSignature({ electronPlatformName: 'linux' }, () =>
    assert.fail('must not invoke codesign'),
  )
})

async function withPluginSources(test) {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-plugin-package-'))
  const source = join(directory, 'source')
  const destination = join(directory, 'resources')
  const windowsLauncher = join(directory, 'native-launcher.exe')
  const manifests = {}
  try {
    await writeFile(windowsLauncher, 'native launcher fixture')
    for (const [name, server] of [
      ['browser-use', 'browser'],
      ['chrome-use', 'chrome'],
      ['computer-use', 'computer'],
    ]) {
      await mkdir(join(source, name, 'tests'), { recursive: true })
      await writeFile(join(source, name, 'tests', 'test.mjs'), 'excluded')
      await writeFile(join(source, name, 'server.mjs'), 'server fixture')
      const manifest = {
        mcpServers: { [server]: { command: './launch.sh', env_vars: ['OPENHARNESS_NODE_BINARY'] } },
      }
      manifests[name] = manifest
      await writeFile(join(source, name, 'mcp.json'), JSON.stringify(manifest))
    }
    await mkdir(join(source, 'computer-use', 'windows', 'bin'), { recursive: true })
    await writeFile(join(source, 'computer-use', 'windows', 'Fixture.cs'), 'test window source')
    for (const file of [
      'OpenHarnessComputerUse.exe',
      'ComputerUseFixture.exe',
      'ComputerUseFixture.pdb',
    ])
      await writeFile(join(source, 'computer-use', 'windows', 'bin', file), file)
    await mkdir(join(source, 'computer-use', 'bin', 'Open Harness Computer Use.app'), {
      recursive: true,
    })
    await test({ source, destination, windowsLauncher, manifests })
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

test('Windows packages all control plugins with native MCP launchers and only the production desktop helper', async () => {
  await withPluginSources(async (options) => {
    await stagePluginResources({ ...options, platform: 'win32' })
    for (const [name, manifest] of Object.entries(options.manifests)) {
      const target = join(options.destination, name)
      assert.equal(await readFile(join(target, 'launch.exe'), 'utf8'), 'native launcher fixture')
      const output = JSON.parse(await readFile(join(target, 'mcp.json'), 'utf8'))
      assert.deepEqual(Object.values(output.mcpServers), [
        {
          command: './launch.exe',
          args: ['--mcp'],
          env_vars: ['OPENHARNESS_NODE_BINARY'],
        },
      ])
      assert.deepEqual(
        JSON.parse(await readFile(join(options.source, name, 'mcp.json'), 'utf8')),
        manifest,
      )
      await assert.rejects(access(join(target, 'tests')))
    }
    const computer = join(options.destination, 'computer-use')
    await access(join(computer, 'windows', 'bin', 'OpenHarnessComputerUse.exe'))
    await assert.rejects(access(join(computer, 'windows', 'bin', 'ComputerUseFixture.exe')))
    await assert.rejects(access(join(computer, 'windows', 'bin', 'ComputerUseFixture.pdb')))
    await assert.rejects(access(join(computer, 'windows', 'Fixture.cs')))
    await assert.rejects(access(join(computer, 'bin')))
  })
})

test('Windows packaging fails before staging when its native desktop helper was not built', async () => {
  await withPluginSources(async (options) => {
    await rm(join(options.source, 'computer-use', 'windows', 'bin', 'OpenHarnessComputerUse.exe'))
    await assert.rejects(stagePluginResources({ ...options, platform: 'win32' }), {
      code: 'ENOENT',
    })
    await assert.rejects(access(options.destination))
  })
})

test('macOS retains its app and shell launchers without shipping a Windows helper', async () => {
  await withPluginSources(async (options) => {
    await stagePluginResources({ ...options, platform: 'darwin' })
    for (const [name, manifest] of Object.entries(options.manifests)) {
      const target = join(options.destination, name)
      assert.deepEqual(JSON.parse(await readFile(join(target, 'mcp.json'), 'utf8')), manifest)
      await assert.rejects(access(join(target, 'launch.exe')))
    }
    await access(join(options.destination, 'computer-use', 'bin', 'Open Harness Computer Use.app'))
    await assert.rejects(access(join(options.destination, 'computer-use', 'windows')))
  })
})
