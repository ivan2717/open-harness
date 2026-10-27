import { execFileSync } from 'node:child_process'
import { cp, lstat, mkdir, readFile, realpath, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'
import { nodeRelease } from './workspace-runtime-node.mjs'
import {
  prepareManagedRuntime,
  pythonRelease,
  verifyManagedRuntime,
  verifyPayloadLinks,
} from './workspace-runtime-bundle.mjs'

/** Optional distributor-supplied, offline standalone Python. Never uses PATH. */
export async function stageWorkspaceRuntime({
  source,
  destination,
  platform = process.platform,
  architecture = process.arch,
  probe = execFileSync,
}) {
  await mkdir(destination, { recursive: true })
  // With no payload, remove an earlier staged runtime rather than accidentally
  // shipping a builder's interpreter or another platform's stale binaries.
  if (!source) {
    await rm(join(destination, 'python'), { recursive: true, force: true })
    await writeFile(
      join(destination, 'runtime.json'),
      `${JSON.stringify({ version: 1, python: null })}\n`,
    )
    return
  }
  if (!isAbsolute(source)) throw new Error('OPENHARNESS_PYTHON_RUNTIME_DIR must be absolute')
  if (!(await lstat(source)).isDirectory())
    throw new Error('Python runtime payload must be a directory')
  const root = await realpath(source)
  const target = resolve(destination)
  if (root === target || root.startsWith(`${target}${sep}`) || target.startsWith(`${root}${sep}`))
    throw new Error('Python runtime source and staging directory must be separate')
  await verifyPayloadLinks(root)
  const staged = join(destination, `.python-${randomUUID()}`)
  const previous = join(destination, `.previous-${randomUUID()}`)
  const installed = join(destination, 'python')
  let movedPrevious = false
  let removePrevious = false
  try {
    await cp(root, staged, { recursive: true, dereference: false, verbatimSymlinks: true })
    const executable = join(staged, ...(platform === 'win32' ? ['python.exe'] : ['bin', 'python3']))
    // Check relocation by launching the copy, not the original. Standard
    // library imports catch incomplete/host-dependent Python installations.
    const result = probe(
      executable,
      [
        '-I',
        '-B',
        '-c',
        'import csv,json,platform,sqlite3,ssl,sys; assert sys.version_info >= (3,10); print(json.dumps({"version":sys.version.split()[0],"platform":sys.platform,"architecture":platform.machine().lower()}))',
      ],
      {
        timeout: 20_000,
        maxBuffer: 32_768,
        encoding: 'utf8',
        windowsHide: true,
        env: { ...process.env, PYTHONHOME: '', PYTHONPATH: '' },
      },
    )
    const {
      version,
      platform: runtimePlatform,
      architecture: runtimeArchitecture,
    } = JSON.parse(result)
    if (typeof version !== 'string' || !/^3\.\d+\.\d+$/.test(version))
      throw new Error('Invalid bundled Python version')
    const expectedArchitectures =
      architecture === 'arm64'
        ? ['arm64', 'aarch64']
        : architecture === 'x64'
          ? ['x86_64', 'amd64']
          : [architecture]
    if (runtimePlatform !== platform || !expectedArchitectures.includes(runtimeArchitecture))
      throw new Error('Python runtime payload does not match the build platform and architecture')
    try {
      await rename(installed, previous)
      movedPrevious = true
    } catch (error) {
      if (error.code !== 'ENOENT') throw error
    }
    try {
      await rename(staged, installed)
      removePrevious = true
    } catch (error) {
      if (movedPrevious) {
        await rename(previous, installed)
        movedPrevious = false
      }
      throw error
    }
    await writeFile(
      join(destination, 'runtime.json'),
      `${JSON.stringify({ version: 1, python: { version, platform, architecture } })}\n`,
    )
  } finally {
    await rm(staged, { recursive: true, force: true })
    if (removePrevious) await rm(previous, { recursive: true, force: true })
  }
}

/** Preserve package-manager links as relative links, so the app is relocatable. */
export async function stageNodeDependencies(source, destination) {
  await verifyPayloadLinks(await realpath(source))
  await cp(source, destination, { recursive: true, dereference: false, verbatimSymlinks: true })
  await verifyPayloadLinks(await realpath(destination))
}

/** Mark the relocated copy, including distributor-supplied offline payloads. */
export async function markManagedPython(
  directory,
  platform = process.platform,
  probe = execFileSync,
) {
  const pythonRoot = join(directory, 'python')
  const executable = join(
    pythonRoot,
    ...(platform === 'win32' ? ['python.exe'] : ['bin', 'python3']),
  )
  const standardLibrary = probe(
    executable,
    ['-I', '-B', '-c', 'import sysconfig; print(sysconfig.get_path("stdlib"))'],
    { encoding: 'utf8', timeout: 10_000, windowsHide: true },
  ).trim()
  const relativeLibrary = relative(await realpath(pythonRoot), await realpath(standardLibrary))
  if (
    !relativeLibrary ||
    isAbsolute(relativeLibrary) ||
    relativeLibrary === '..' ||
    relativeLibrary.startsWith(`..${sep}`)
  )
    throw new Error('Managed Python standard library must be inside the bundled runtime')
  await writeFile(
    join(standardLibrary, 'EXTERNALLY-MANAGED'),
    '[externally-managed]\nError=This Python belongs to Open Harness. Install additional packages in a task virtual environment.\n',
  )
}

/** Build a complete runtime before replacing the shipped resources. */
export async function stageManagedWorkspaceRuntime({
  source = process.env.OPENHARNESS_PYTHON_RUNTIME_DIR,
  nodeSource = process.env.OPENHARNESS_NODE_RUNTIME_DIR,
  destination,
  cache = `${destination}-cache`,
  platform = process.platform,
  architecture = process.arch,
}) {
  const runtime = await prepareManagedRuntime({ source, nodeSource, cache, platform, architecture })
  const marker = join(destination, '.bundle-fingerprint')
  // An explicitly supplied payload may have changed without its path changing.
  if (
    !source &&
    !nodeSource &&
    (await readFile(marker, 'utf8').catch(() => '')) === runtime.fingerprint
  ) {
    try {
      await verifyPayloadLinks(await realpath(join(destination, 'python')))
      await verifyPayloadLinks(await realpath(join(destination, 'node')))
      verifyManagedRuntime(destination, platform)
      return
    } catch {
      /* Repair incomplete staged resources from the verified build cache. */
    }
  }
  const staging = `${destination}-${randomUUID()}`
  const previous = `${destination}-previous-${randomUUID()}`
  let movedPrevious = false
  try {
    await stageWorkspaceRuntime({
      source: runtime.python,
      destination: staging,
      platform,
      architecture,
    })
    await stageNodeDependencies(runtime.node, join(staging, 'node'))
    verifyManagedRuntime(staging, platform)
    await markManagedPython(staging, platform)
    const configuration = join(dirname(fileURLToPath(import.meta.url)), 'workspace-runtime')
    await cp(join(configuration, 'requirements.txt'), join(staging, 'requirements.txt'))
    await writeFile(
      join(staging, 'NOTICE.txt'),
      [
        'Open Harness managed scripting runtime',
        source
          ? 'Distributor-supplied standalone Python. See runtime.json for the verified interpreter version.'
          : `CPython ${pythonRelease.version}; standalone build ${pythonRelease.release} from https://github.com/astral-sh/python-build-standalone.`,
        'Python and bundled libraries retain their upstream licenses in the python directory and *.dist-info directories.',
        nodeSource
          ? 'Distributor-supplied standalone Node. Node license is included in node/LICENSE.'
          : `Node ${nodeRelease} from https://nodejs.org/dist/. Node license is included in node/LICENSE.`,
        'pnpm and Node library licenses are included in node/node_modules. No npm CLI is shipped.',
        'Pinned Python package hashes: requirements.txt. Pinned pnpm dependencies: node/pnpm-lock.yaml.',
        'Shared dependencies are application resources. Additional dependencies belong in task environments.',
        '',
      ].join('\n'),
    )
    await writeFile(join(staging, '.bundle-fingerprint'), runtime.fingerprint)
    try {
      await rename(destination, previous)
      movedPrevious = true
    } catch (error) {
      if (error.code !== 'ENOENT') throw error
    }
    try {
      await rename(staging, destination)
    } catch (error) {
      if (movedPrevious) {
        await rename(previous, destination)
        movedPrevious = false
      }
      throw error
    }
  } finally {
    await rm(staging, { recursive: true, force: true })
    if (movedPrevious) await rm(previous, { recursive: true, force: true })
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  await stageManagedWorkspaceRuntime({
    destination: join(desktop, 'resources', 'workspace-runtime'),
  })
}
