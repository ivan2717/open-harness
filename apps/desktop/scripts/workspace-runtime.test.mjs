import { mkdtemp, mkdir, readFile, readlink, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from 'vitest'
import {
  markManagedPython,
  stageNodeDependencies,
  stageWorkspaceRuntime,
} from './workspace-runtime.mjs'

async function withFixture(run) {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-runtime-package-'))
  try {
    const source = join(directory, 'source')
    const destination = join(directory, 'resources')
    await mkdir(join(source, 'bin'), { recursive: true })
    await writeFile(join(source, 'bin', 'python3'), 'standalone fixture')
    await run({ directory, source, destination })
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

test('offline Python payload is validated after relocation before replacing the previous copy', async () => {
  await withFixture(async (options) => {
    let stagedBinary
    await stageWorkspaceRuntime({
      ...options,
      platform: 'darwin',
      architecture: 'arm64',
      probe: (binary, args) => {
        stagedBinary = binary
        expect(binary).toContain('.python-')
        expect(args.slice(0, 3)).toEqual(['-I', '-B', '-c'])
        return '{"version":"3.12.7","platform":"darwin","architecture":"arm64"}'
      },
    })
    expect(stagedBinary).toBeTruthy()
    expect(await readFile(join(options.destination, 'python', 'bin', 'python3'), 'utf8')).toBe(
      'standalone fixture',
    )
    await expect(
      stageWorkspaceRuntime({
        ...options,
        platform: 'darwin',
        architecture: 'arm64',
        probe: () => {
          throw new Error('not relocatable')
        },
      }),
    ).rejects.toThrow('not relocatable')
    expect(await readFile(join(options.destination, 'python', 'bin', 'python3'), 'utf8')).toBe(
      'standalone fixture',
    )
  })
})

test('a build without a Python payload removes stale platform-specific interpreters', async () => {
  await withFixture(async (options) => {
    await mkdir(join(options.destination, 'python'), { recursive: true })
    await writeFile(join(options.destination, 'python', 'old.exe'), 'old')
    await stageWorkspaceRuntime({ destination: options.destination })
    await expect(readFile(join(options.destination, 'python', 'old.exe'))).rejects.toThrow()
    expect(
      JSON.parse(await readFile(join(options.destination, 'runtime.json'), 'utf8')).python,
    ).toBeNull()
  })
})

test('Windows uses its native executable and rejects a payload for another architecture', async () => {
  await withFixture(async (options) => {
    await writeFile(join(options.source, 'python.exe'), 'Windows fixture')
    await stageWorkspaceRuntime({
      ...options,
      platform: 'win32',
      architecture: 'x64',
      probe: (binary) => {
        expect(binary.endsWith('python.exe')).toBe(true)
        return '{"version":"3.12.7","platform":"win32","architecture":"amd64"}'
      },
    })
    await expect(
      stageWorkspaceRuntime({
        ...options,
        platform: 'win32',
        architecture: 'x64',
        probe: () => '{"version":"3.12.7","platform":"win32","architecture":"arm64"}',
      }),
    ).rejects.toThrow('build platform and architecture')
    expect(await readFile(join(options.destination, 'python', 'python.exe'), 'utf8')).toBe(
      'Windows fixture',
    )
  })
})

test('runtime packaging rejects symlinks into the builder filesystem', async () => {
  if (process.platform === 'win32') return // Creating symlinks needs developer mode on Windows.
  await withFixture(async (options) => {
    const external = join(options.directory, 'private.txt')
    await writeFile(external, 'must not be packaged')
    await symlink(external, join(options.source, 'escape'))
    await expect(
      stageWorkspaceRuntime({ ...options, probe: () => '{"version":"3.12.7"}' }),
    ).rejects.toThrow('escaping symbolic link')
  })
})

test('offline payload copies receive the same managed-install protection without touching the source', async () => {
  await withFixture(async ({ source, destination }) => {
    const stdlib = join(destination, 'python', 'lib', 'python3.12')
    await mkdir(stdlib, { recursive: true })
    await markManagedPython(destination, 'darwin', () => stdlib)
    expect(await readFile(join(stdlib, 'EXTERNALLY-MANAGED'), 'utf8')).toContain(
      'task virtual environment',
    )
    await expect(readFile(join(source, 'EXTERNALLY-MANAGED'))).rejects.toThrow()
    await expect(markManagedPython(destination, 'darwin', () => source)).rejects.toThrow(
      'inside the bundled runtime',
    )
  })
})

test('Node pnpm links stay relative and usable after the build cache is removed', async () => {
  if (process.platform === 'win32') return
  await withFixture(async ({ source, destination }) => {
    await mkdir(join(source, 'node_modules', '.bin'), { recursive: true })
    await mkdir(join(source, 'node_modules', 'pnpm', 'bin'), { recursive: true })
    await writeFile(join(source, 'node_modules', 'pnpm', 'bin', 'pnpm.cjs'), 'portable pnpm')
    await symlink('../pnpm/bin/pnpm.cjs', join(source, 'node_modules', '.bin', 'pnpm'))
    await stageNodeDependencies(source, destination)
    const copiedLink = join(destination, 'node_modules', '.bin', 'pnpm')
    expect(await readlink(copiedLink)).toBe('../pnpm/bin/pnpm.cjs')
    await rm(source, { recursive: true, force: true })
    expect(await readFile(copiedLink, 'utf8')).toBe('portable pnpm')
  })
})

test('Node staging rejects links that point at builder files', async () => {
  if (process.platform === 'win32') return
  await withFixture(async ({ directory, source, destination }) => {
    const external = join(directory, 'host-package.js')
    await writeFile(external, 'host dependency')
    await symlink(external, join(source, 'escaping-package'))
    await expect(stageNodeDependencies(source, destination)).rejects.toThrow(
      'escaping symbolic link',
    )
  })
})

test('Node staging rejects absolute internal directory links, including Windows junctions', async () => {
  await withFixture(async ({ source, destination }) => {
    const actual = join(source, 'packages', 'fixture')
    await mkdir(actual, { recursive: true })
    await writeFile(join(actual, 'index.js'), 'package fixture')
    await symlink(actual, join(source, 'fixture'), 'junction')
    await expect(stageNodeDependencies(source, destination)).rejects.toThrow(
      'escaping symbolic link',
    )
  })
})
