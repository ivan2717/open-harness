import { spawnSync } from 'node:child_process'
import { chmod, copyFile, mkdir } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const workspace = resolve(desktop, '../..')
export function backendBuildPlan({ platform, arch, release, chromeHostOnly = false }) {
  if (chromeHostOnly && platform !== 'win32') {
    throw new Error('The Chrome executable launcher is packaged only on Windows')
  }
  const windowsTarget = release && platform === 'win32' ? 'x86_64-pc-windows-msvc' : undefined
  if (windowsTarget && arch !== 'x64') {
    throw new Error('Windows release packaging currently requires a Windows x64 host')
  }
  return {
    windowsTarget,
    arguments: [
      'build',
      '--locked',
      ...(!chromeHostOnly ? ['-p', 'api'] : []),
      ...(platform === 'win32' ? ['-p', 'chrome-native-host'] : []),
      ...(platform === 'win32' && !chromeHostOnly ? ['-p', 'agent-local'] : []),
      ...(release ? ['--release'] : []),
      ...(windowsTarget ? ['--target', windowsTarget] : []),
    ],
    filenames: [
      ...(release && !chromeHostOnly ? [platform === 'win32' ? 'api.exe' : 'api'] : []),
      ...(platform === 'win32' ? ['openharness-chrome-host.exe'] : []),
      ...(platform === 'win32' && !chromeHostOnly ? ['openharness-sandbox.exe'] : []),
    ],
  }
}

async function buildBackend() {
  const release = process.argv[2] === 'release'
  const {
    windowsTarget,
    arguments: args,
    filenames,
  } = backendBuildPlan({
    platform: process.platform,
    arch: process.arch,
    release,
    chromeHostOnly: process.argv.includes('--chrome-host-only'),
  })
  const environment = { ...process.env }
  if (windowsTarget) {
    // Keep user flags, but make the shipped backend independent of an installed
    // Visual C++ redistributable. An explicit target separates host build scripts
    // and procedural macros from the statically linked application dependencies.
    const flags =
      environment.CARGO_ENCODED_RUSTFLAGS !== undefined
        ? environment.CARGO_ENCODED_RUSTFLAGS.split('\u001f').filter(Boolean)
        : (environment.RUSTFLAGS ?? environment.CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS ?? '')
            .split(/\s+/)
            .filter(Boolean)
    environment.CARGO_ENCODED_RUSTFLAGS = [...flags, '-C', 'target-feature=+crt-static'].join(
      '\u001f',
    )
  }
  const build = spawnSync('cargo', args, {
    cwd: workspace,
    stdio: 'inherit',
    env: environment,
  })
  if (build.error) throw build.error
  if (build.status !== 0) process.exit(build.status ?? 1)
  if (release || process.platform === 'win32') {
    const targetDirectory = process.env.CARGO_TARGET_DIR
      ? resolve(workspace, process.env.CARGO_TARGET_DIR)
      : resolve(workspace, 'target')
    const resources = resolve(desktop, 'resources/bin')
    await mkdir(resources, { recursive: true })
    for (const filename of filenames) {
      await copyFile(
        resolve(
          targetDirectory,
          ...(windowsTarget ? [windowsTarget] : []),
          release ? 'release' : 'debug',
          filename,
        ),
        resolve(resources, filename),
      )
      if (process.platform !== 'win32') await chmod(resolve(resources, filename), 0o755)
    }
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await buildBackend()
}
