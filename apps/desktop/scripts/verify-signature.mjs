import { spawnSync } from 'node:child_process'
import { join } from 'node:path'

// electron-builder signs nested executables before this hook runs. A valid
// signature must bind our bundle, not retain Electron's upstream linker identity.
export function verifyMacSignature(context, run = spawnSync) {
  if (context.electronPlatformName !== 'darwin') return
  const { productFilename, macBundleIdentifier } = context.packager.appInfo
  const appPath = join(context.appOutDir, `${productFilename}.app`)
  function codesign(args, target = appPath) {
    const result = run('/usr/bin/codesign', [...args, target], {
      encoding: 'utf8',
      timeout: 120_000,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    if (result.error) throw result.error
    if (result.status !== 0) {
      throw new Error(
        `Signature validation failed for ${target}: ${result.stderr || result.stdout}`,
      )
    }
    return `${result.stdout ?? ''}\n${result.stderr ?? ''}`
  }
  codesign(['--verify', '--deep', '--strict', '--verbose=2'])
  const signature = codesign(['--display', '--verbose=2'])
  const identifier = /^Identifier=(.+)$/m.exec(signature)?.[1]?.trim()
  if (identifier !== macBundleIdentifier) {
    throw new Error(
      `Signature identity mismatch: expected ${macBundleIdentifier}, received ${identifier ?? 'none'}`,
    )
  }
  const computerApp = join(
    appPath,
    'Contents',
    'Resources',
    'plugins',
    'computer-use',
    'bin',
    'Open Harness Computer Use.app',
  )
  codesign(['--verify', '--strict', '--verbose=2'], computerApp)
  const computerSignature = codesign(['--display', '--verbose=2'], computerApp)
  const computerIdentifier = /^Identifier=(.+)$/m.exec(computerSignature)?.[1]?.trim()
  if (computerIdentifier !== 'dev.openharness.computeruse') {
    throw new Error(
      `Computer Use signature identity mismatch: expected dev.openharness.computeruse, received ${computerIdentifier ?? 'none'}`,
    )
  }
  if (context.packager.platformSpecificBuildOptions?.sign === './scripts/sign-local-mac.mjs') {
    const requirements = [appPath, computerApp].map((target) =>
      codesign(['--display', '-r-'], target),
    )
    const certificates = requirements.map((text) =>
      /certificate leaf = H"([a-f0-9]{40})"/i.exec(text)?.[1]?.toLowerCase(),
    )
    if (!certificates[0] || certificates[0] !== certificates[1])
      throw new Error(
        'Local desktop and Computer Use must share a stable certificate-backed signing identity',
      )
  }
}

export default function afterSign(context) {
  verifyMacSignature(context)
}
