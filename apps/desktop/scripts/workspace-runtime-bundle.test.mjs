import { createHash } from 'node:crypto'
import { mkdirSync, writeFileSync } from 'node:fs'
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test, vi } from 'vitest'
import {
  pythonDownload,
  cleanManagedNodeMetadata,
  reusableRuntime,
  validateArchiveEntries,
  verifiedDownload,
  prepareManagedRuntime,
} from './workspace-runtime-bundle.mjs'
import { stageNodeDependencies } from './workspace-runtime.mjs'

test('managed Python archive is pinned for each supported packaging target', () => {
  for (const [platform, arch] of [
    ['darwin', 'arm64'],
    ['darwin', 'x64'],
    ['win32', 'x64'],
    ['linux', 'x64'],
    ['linux', 'arm64'],
  ]) {
    const artifact = pythonDownload(platform, arch)
    expect(artifact.sha256).toMatch(/^[a-f0-9]{64}$/)
    expect(artifact.url).toContain('/20261021/cpython-3.12.15%2B20261021-')
  }
  expect(() => pythonDownload('win32', 'arm64')).toThrow('Supply OPENHARNESS_PYTHON_RUNTIME_DIR')
})

test('archive extraction accepts only the known Python prefix', () => {
  expect(() => validateArchiveEntries('python/\npython/bin/python3\n')).not.toThrow()
  for (const name of [
    '../../secret',
    '/python/bin/python',
    'python/../secret',
    'python/\\..\\secret',
    'other/file',
  ])
    expect(() => validateArchiveEntries(name)).toThrow('Unexpected path')
})

test('download cache is verified before reuse and rejects corrupt bytes without replacing a valid cache', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-managed-download-'))
  try {
    const payload = 'verified interpreter archive fixture'
    const sha256 = createHash('sha256').update(payload).digest('hex')
    const destination = join(directory, 'python.tar.gz')
    let calls = 0
    const options = {
      url: 'https://example.invalid/python.tar.gz',
      sha256,
      destination,
      fetchFile: async () => {
        calls++
        return new Response(payload)
      },
    }
    await verifiedDownload(options)
    await verifiedDownload(options)
    expect(calls).toBe(1)
    await writeFile(destination, 'corrupted cache')
    await expect(
      verifiedDownload({ ...options, fetchFile: async () => new Response('wrong download') }),
    ).rejects.toThrow('SHA-256 mismatch')
    expect(await readFile(destination, 'utf8')).toBe('corrupted cache')
    expect(await readdir(directory)).toEqual(['python.tar.gz'])
    await verifiedDownload(options)
    expect(await readFile(destination, 'utf8')).toBe(payload)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test('a matching cache marker does not mask missing interpreters or broken dependencies', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-runtime-ready-'))
  try {
    await writeFile(join(directory, '.ready'), 'fingerprint')
    await mkdir(join(directory, 'python'))
    await mkdir(join(directory, 'node'))
    const calls = []
    const healthy = (executable, args) => {
      calls.push({ executable, args })
      return ''
    }
    expect(await reusableRuntime(directory, 'fingerprint', 'darwin', healthy)).toBe(true)
    expect(calls[0].args.slice(0, 3)).toEqual(['-I', '-B', '-c'])
    expect(calls[1].args[1]).toContain('pnpm.cjs')
    expect(
      await reusableRuntime(directory, 'fingerprint', 'darwin', () => {
        throw new Error('interpreter deleted')
      }),
    ).toBe(false)
    expect(
      await reusableRuntime(directory, 'fingerprint', 'darwin', (executable) => {
        if (executable === join(directory, 'node', 'bin', 'node'))
          throw new Error('node package deleted')
      }),
    ).toBe(false)
    expect(
      await reusableRuntime(directory, 'old-fingerprint', 'darwin', () => {
        throw new Error('must not probe stale cache')
      }),
    ).toBe(false)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test('managed Node bundle excludes pnpm metadata and wrappers containing build paths', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-portable-pnpm-'))
  try {
    const modules = join(directory, 'node_modules')
    await mkdir(join(modules, '.bin'), { recursive: true })
    await mkdir(join(modules, 'pnpm', 'bin'), { recursive: true })
    await writeFile(join(modules, '.modules.yaml'), 'storeDir: /private/builder/store')
    await writeFile(join(modules, '.bin', 'pnpm'), 'NODE_PATH=/private/builder/node_modules')
    await writeFile(join(modules, 'pnpm', 'bin', 'pnpm.cjs'), 'portable CLI')
    await cleanManagedNodeMetadata(directory)
    expect(await readdir(modules)).toEqual(['pnpm'])
    expect(await readFile(join(modules, 'pnpm', 'bin', 'pnpm.cjs'), 'utf8')).toBe('portable CLI')
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test('offline Node provisioning requests a portable hoisted install that survives cache removal', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'openharness-hoisted-runtime-'))
  try {
    const source = join(directory, 'source')
    const cache = join(directory, 'cache')
    const destination = join(directory, 'resources')
    const pnpm = join(directory, 'pnpm.cjs')
    await mkdir(join(source, 'bin'), { recursive: true })
    await writeFile(join(source, 'bin', 'node'), 'standalone node fixture')
    await writeFile(join(source, 'LICENSE'), 'Node license')
    await writeFile(pnpm, 'build-time pnpm fixture')
    vi.stubEnv('npm_execpath', pnpm)
    let installs = 0
    const runtime = await prepareManagedRuntime({
      cache,
      source: join(directory, 'offline-python'),
      nodeSource: source,
      platform: 'darwin',
      architecture: 'arm64',
      run: (executable, args) => {
        if (args[0] === '-e') {
          expect(executable).not.toBe(process.execPath)
          return '{"version":"24.22.1","platform":"darwin","architecture":"arm64"}'
        }
        expect(args[0]).toBe(pnpm)
        expect(args).toContain('--config.node-linker=hoisted')
        expect(args).toContain('--config.package-import-method=copy')
        expect(args).toContain('--frozen-lockfile')
        expect(args).toContain('--ignore-scripts')
        installs++
        const modules = join(args[args.indexOf('--dir') + 1], 'node_modules')
        mkdirSync(join(modules, 'fixture'), { recursive: true })
        writeFileSync(join(modules, 'fixture', 'index.js'), 'module.exports = "portable"')
        return ''
      },
    })
    expect(installs).toBe(1)
    await stageNodeDependencies(runtime.node, destination)
    await rm(cache, { recursive: true, force: true })
    expect(await readFile(join(destination, 'node_modules', 'fixture', 'index.js'), 'utf8')).toBe(
      'module.exports = "portable"',
    )
    expect(await readFile(join(destination, 'bin', 'node'), 'utf8')).toBe('standalone node fixture')
  } finally {
    vi.unstubAllEnvs()
    await rm(directory, { recursive: true, force: true })
  }
})
