import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'vitest'
import { directoryBuildArguments } from './package-dir.mjs'
import { verifyMacSignature } from './verify-signature.mjs'

const context = {
  electronPlatformName: 'darwin',
  appOutDir: '/build/mac-arm64',
  packager: {
    appInfo: { productFilename: 'Open Harness', macBundleIdentifier: 'dev.openharness.desktop' },
  },
}

test('only the local macOS directory build requests ad-hoc signing', async () => {
  assert.deepEqual(directoryBuildArguments('darwin'), ['--dir', '-c.mac.identity=-'])
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

test('the post-sign hook verifies nested code and the actual bundle identity', () => {
  const calls = []
  verifyMacSignature(context, (command, args, options) => {
    calls.push({ command, args, options })
    return { status: 0, stdout: '', stderr: `Identifier=${args.at(-1).endsWith('Open Harness Computer Use.app') ? 'dev.openharness.computeruse' : 'dev.openharness.desktop'}\n` }
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
  assert.deepEqual(calls[2].args, ['--verify', '--strict', '--verbose=2', '/build/mac-arm64/Open Harness.app/Contents/Resources/plugins/computer-use/bin/Open Harness Computer Use.app'])
  assert.equal(calls[3].args[0], '--display')
})

test('the independent Computer Use app must have its own signing identity', () => {
  assert.throws(
    () => verifyMacSignature(context, () => ({ status: 0, stderr: 'Identifier=dev.openharness.desktop\n' })),
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
