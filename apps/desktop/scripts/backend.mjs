import { spawnSync } from 'node:child_process'
import { chmod, copyFile, mkdir } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const workspace = resolve(desktop, '../..')
const release = process.argv[2] === 'release'
const chromeHostOnly = process.argv.includes('--chrome-host-only')
if (chromeHostOnly && process.platform !== 'win32') {
  throw new Error('The Chrome executable launcher is packaged only on Windows')
}
const windowsTarget = release && process.platform === 'win32' ? 'x86_64-pc-windows-msvc' : undefined
if (windowsTarget && process.arch !== 'x64') {
  throw new Error('Windows release packaging currently requires a Windows x64 host')
}
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
const build = spawnSync(
  'cargo',
  [
    'build',
    '--locked',
    ...(!chromeHostOnly ? ['-p', 'api'] : []),
    ...(process.platform === 'win32' ? ['-p', 'chrome-native-host'] : []),
    ...(release ? ['--release'] : []),
    ...(windowsTarget ? ['--target', windowsTarget] : []),
  ],
  {
    cwd: workspace,
    stdio: 'inherit',
    env: environment,
  },
)
if (build.error) throw build.error
if (build.status !== 0) process.exit(build.status ?? 1)
if (release || process.platform === 'win32') {
  const targetDirectory = process.env.CARGO_TARGET_DIR
    ? resolve(workspace, process.env.CARGO_TARGET_DIR)
    : resolve(workspace, 'target')
  const resources = resolve(desktop, 'resources/bin')
  await mkdir(resources, { recursive: true })
  const filenames = [
    ...(release && !chromeHostOnly ? [process.platform === 'win32' ? 'api.exe' : 'api'] : []),
    ...(process.platform === 'win32' ? ['openharness-chrome-host.exe'] : []),
  ]
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
