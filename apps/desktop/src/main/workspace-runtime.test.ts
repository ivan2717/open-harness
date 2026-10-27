import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, test } from 'vitest'
import {
  discoverWorkspaceRuntime,
  parsePythonProbe,
  prepareWorkspaceRuntime,
  pythonCandidates,
} from './workspace-runtime'

const sample = {
  executable: '/opt/python/bin/python3',
  version: '3.12.7',
  libraries: [{ name: 'pandas', version: '2.2.3' }],
  read_paths: ['/opt/python', '/opt/python/lib/python3.12'],
}

describe('workspace runtime discovery', () => {
  test('reports only validated Python 3 metadata and uses isolated invocation', () => {
    expect(parsePythonProbe(JSON.stringify(sample), 'darwin', 'system')).toEqual({
      ...sample,
      args: ['-I', '-B'],
      environment: {},
      source: 'system',
    })
    expect(() =>
      parsePythonProbe(JSON.stringify({ ...sample, version: '2.7.18' }), 'darwin', 'system'),
    ).toThrow()
    expect(() =>
      parsePythonProbe(JSON.stringify({ ...sample, executable: 'python3' }), 'darwin', 'system'),
    ).toThrow()
    expect(() =>
      parsePythonProbe(JSON.stringify({ ...sample, read_paths: ['relative'] }), 'darwin', 'system'),
    ).toThrow()
    expect(() =>
      parsePythonProbe(
        JSON.stringify({ ...sample, libraries: [{ name: 'pandas' }] }),
        'darwin',
        'system',
      ),
    ).toThrow()
  })

  test('accepts native Windows interpreter paths and rejects relative launcher paths', () => {
    expect(
      parsePythonProbe(
        JSON.stringify({
          ...sample,
          executable: 'C:\\Python312\\python.exe',
          read_paths: ['C:\\Python312'],
        }),
        'win32',
        'system',
      ).executable,
    ).toBe('C:\\Python312\\python.exe')
    expect(() =>
      parsePythonProbe(
        JSON.stringify({
          ...sample,
          executable: '\\Python312\\python.exe',
          read_paths: ['relative'],
        }),
        'win32',
        'system',
      ),
    ).toThrow()
  })

  test('keeps Node available when Python probes fail and does not run a shell', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      const nodeBinary = await nodeFixture(root)
      const binary = join(root, 'python3')
      await writeFile(binary, '')
      const attempted: string[][] = []
      const result = await discoverWorkspaceRuntime({
        directory: root,
        platform: 'darwin',
        resourcesDirectory: root,
        environment: { OPENHARNESS_PYTHON_BINARY: binary, PATH: '' },
        probe: async (executable, args) => {
          attempted.push([executable, ...args])
          if (executable === nodeBinary) return nodeMetadata()
          throw new Error('broken interpreter')
        },
      })
      expect(result.python).toBeNull()
      expect(result.node?.environment).toEqual({})
      expect(result.node?.executable).toBe(nodeBinary)
      expect(result.node?.read_paths).toEqual([join(root, 'workspace-runtime', 'node')])
      expect(attempted.find(([exe]) => exe === binary)?.slice(0, 4)).toEqual([
        binary,
        '-I',
        '-B',
        '-c',
      ])
      expect(result.notes.join(' ')).toContain('No interpreter or packages were downloaded')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('does not discover arbitrary PATH Python without explicit opt-in', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      await writeFile(join(root, 'python3'), '')
      const options = {
        platform: 'darwin' as const,
        resourcesDirectory: root,
        environment: { PATH: root },
      }
      expect(await pythonCandidates(options)).toEqual([])
      expect(
        await pythonCandidates({
          ...options,
          environment: { PATH: root, OPENHARNESS_ALLOW_SYSTEM_PYTHON: '1' },
        }),
      ).toContainEqual({ executable: join(root, 'python3'), args: [], source: 'system' })
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('does not substitute Electron or PATH Node when the bundled executable is unavailable', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      await writeFile(join(root, 'node'), 'unrelated PATH Node')
      const result = await discoverWorkspaceRuntime({
        directory: root,
        platform: 'darwin',
        resourcesDirectory: root,
        environment: { PATH: root },
        probe: async () => {
          throw new Error('No interpreter should be invoked')
        },
      })
      expect(result.node).toBeNull()
      expect(result.notes.join(' ')).toContain('standalone Node runtime is missing or unavailable')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('rejects a bundled Node built for another platform or architecture', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      await nodeFixture(root)
      for (const metadata of [
        { version: '24.22.1', platform: 'win32', architecture: process.arch },
        { version: '24.22.1', platform: 'darwin', architecture: 'unsupported' },
        { version: '18.0.0', platform: 'darwin', architecture: process.arch },
      ]) {
        const result = await discoverWorkspaceRuntime({
          directory: root,
          platform: 'darwin',
          resourcesDirectory: root,
          environment: { PATH: '' },
          probe: async () => JSON.stringify(metadata),
        })
        expect(result.node).toBeNull()
      }
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('reports bundled pnpm as a Node command and advertises only installed libraries', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      const nodeBinary = await nodeFixture(root)
      const modules = join(root, 'workspace-runtime', 'node', 'node_modules')
      await mkdir(join(modules, 'pnpm', 'bin'), { recursive: true })
      await writeFile(
        join(modules, 'pnpm', 'package.json'),
        JSON.stringify({ name: 'pnpm', version: '10.13.1' }),
      )
      await writeFile(join(modules, 'pnpm', 'bin', 'pnpm.cjs'), '')
      const result = await discoverWorkspaceRuntime({
        directory: root,
        platform: 'darwin',
        resourcesDirectory: root,
        environment: { PATH: '' },
        probe: async () => nodeMetadata(),
      })
      expect(result.node?.package_manager).toEqual({
        name: 'pnpm',
        executable: nodeBinary,
        args: [join(modules, 'pnpm', 'bin', 'pnpm.cjs')],
        environment: {},
      })
      expect(result.node?.module_paths).toEqual([modules])
      expect(result.node?.libraries).toContainEqual({ name: 'pnpm', version: '10.13.1' })
      expect(result.node?.libraries.some(({ name }) => name === 'csv-parse')).toBe(false)
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  test('prefers an offline bundled Python and writes a host-owned manifest', async () => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-runtime-'))
    try {
      const nodeBinary = await nodeFixture(root)
      const pythonDirectory = join(root, 'workspace-runtime', 'python')
      const binary = join(pythonDirectory, 'bin', 'python3')
      await mkdir(join(pythonDirectory, 'bin'), { recursive: true })
      await writeFile(binary, '')
      const options = {
        directory: join(root, 'state'),
        platform: 'darwin' as const,
        resourcesDirectory: root,
        environment: { PATH: '' },
        probe: async (executable: string) =>
          executable === nodeBinary
            ? nodeMetadata()
            : JSON.stringify({ ...sample, executable: binary, read_paths: [pythonDirectory] }),
      }
      const environment = await prepareWorkspaceRuntime(options)
      const manifest = JSON.parse(
        await readFile(environment.OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST!, 'utf8'),
      )
      expect(manifest.python.source).toBe('bundled')
      expect(manifest.python.executable).toBe(binary)
      expect(manifest.node.module_paths).toEqual([])
      expect(await prepareWorkspaceRuntime(options)).toEqual(environment)
      expect((await pythonCandidates(options))[0].executable).toBe(binary)
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })
})

function nodeMetadata(platform = 'darwin') {
  return JSON.stringify({ version: '24.22.1', platform, architecture: process.arch })
}

async function nodeFixture(root: string) {
  const directory = join(root, 'workspace-runtime', 'node', 'bin')
  await mkdir(directory, { recursive: true })
  const binary = join(directory, 'node')
  await writeFile(binary, 'standalone Node fixture')
  return binary
}
