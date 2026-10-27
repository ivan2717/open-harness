import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from 'vitest'
import {
  nodeDownload,
  nodeExecutable,
  nodeRelease,
  prepareNodeBinary,
  validateNodeArchiveEntries,
  verifyNodeBinary,
} from './workspace-runtime-node.mjs'

test('independent Node archives are pinned on macOS, Windows and Linux', () => {
  for (const [platform, architecture] of [
    ['darwin', 'arm64'],
    ['darwin', 'x64'],
    ['win32', 'x64'],
    ['linux', 'x64'],
    ['linux', 'arm64'],
  ]) {
    const artifact = nodeDownload(platform, architecture)
    expect(artifact.sha256).toMatch(/^[a-f0-9]{64}$/)
    expect(artifact.url).toBe(`https://nodejs.org/dist/v${nodeRelease}/${artifact.filename}`)
    expect(nodeExecutable('/runtime', platform)).toBe(
      platform === 'win32' ? join('/runtime', 'node.exe') : join('/runtime', 'bin', 'node'),
    )
  }
  expect(() => nodeDownload('win32', 'arm64')).toThrow('OPENHARNESS_NODE_RUNTIME_DIR')
})

test('Node archives cannot escape their known extraction prefix', () => {
  expect(() =>
    validateNodeArchiveEntries('node-fixture/bin/node\nnode-fixture/LICENSE\n', 'node-fixture'),
  ).not.toThrow()
  for (const entry of [
    '../secret',
    'node-fixture/../secret',
    '/node-fixture/bin/node',
    'node-fixture\\node.exe',
  ])
    expect(() => validateNodeArchiveEntries(entry, 'node-fixture')).toThrow('Unexpected path')
})

test('offline Node is validated after copying and excludes npm and unrelated files', async () => {
  const root = await mkdtemp(join(tmpdir(), 'openharness-node-'))
  try {
    const source = join(root, 'source')
    const destination = join(root, 'destination')
    await mkdir(join(source, 'bin'), { recursive: true })
    await writeFile(join(source, 'bin', 'node'), 'node fixture')
    await writeFile(join(source, 'LICENSE'), 'upstream license')
    await writeFile(join(source, 'npm'), 'do not package npm')
    await prepareNodeBinary({
      source,
      destination,
      platform: 'darwin',
      architecture: 'arm64',
      run: (executable, args, options) => {
        expect(executable).toBe(join(destination, 'bin', 'node'))
        expect(args[0]).toBe('-e')
        expect(options.env.NODE_OPTIONS).toBe('')
        return '{"version":"24.22.1","platform":"darwin","architecture":"arm64"}'
      },
    })
    expect(await readFile(join(destination, 'bin', 'node'), 'utf8')).toBe('node fixture')
    expect(await readFile(join(destination, 'LICENSE'), 'utf8')).toBe('upstream license')
    await expect(readFile(join(destination, 'npm'))).rejects.toThrow()
    expect(() =>
      verifyNodeBinary(
        destination,
        'win32',
        'x64',
        () => '{"version":"24.22.1","platform":"darwin","architecture":"arm64"}',
      ),
    ).toThrow('does not match')
    if (process.platform !== 'win32') {
      await rm(join(source, 'bin', 'node'))
      await symlink(join(root, 'external-node'), join(source, 'bin', 'node'))
      await writeFile(join(root, 'external-node'), 'outside')
      await expect(
        prepareNodeBinary({ source, destination, platform: 'darwin', architecture: 'arm64' }),
      ).rejects.toThrow('within the supplied payload')
    }
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
