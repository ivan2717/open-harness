import { execFileSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from 'vitest'
import {
  applySourcePatch,
  fileSha256,
  verifyWindowsNodeBuild,
  windowsNodeFingerprint,
  windowsNodeManifest,
} from './workspace-runtime-node-windows.mjs'
import { prepareNodeBinary } from './workspace-runtime-node.mjs'

test.each([false, true])(
  'applies an archive-relative patch without changing the parent repository (nested: %s)',
  async (nested) => {
    const root = await mkdtemp(join(tmpdir(), 'openharness-node-patch-'))
    const source = join(root, 'cache', 'source directory')
    const parentFile = join(root, 'value.txt')
    const patch = join(root, 'source.patch')
    const git = (args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' })
    try {
      await mkdir(source, { recursive: true })
      await writeFile(parentFile, 'parent repository\n')
      let staged
      if (nested) {
        git(['init', '--quiet'])
        git(['config', 'core.autocrlf', 'true'])
        git(['-c', 'core.autocrlf=false', 'add', 'value.txt'])
        staged = git(['ls-files', '--stage'])
      }
      await writeFile(join(source, 'value.txt'), 'before\n')
      await writeFile(
        patch,
        'diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-before\n+after\n',
      )

      applySourcePatch(source, patch)

      expect(await readFile(join(source, 'value.txt'), 'utf8')).toBe('after\n')
      expect(await readFile(parentFile, 'utf8')).toBe('parent repository\n')
      if (nested) expect(git(['ls-files', '--stage'])).toBe(staged)
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  },
)

test('Windows runtime provenance survives copying and rejects stale or corrupted caches', async () => {
  const root = await mkdtemp(join(tmpdir(), 'openharness-node-backport-'))
  try {
    await writeFile(join(root, 'node.exe'), 'fixture executable')
    await writeFile(join(root, 'LICENSE'), 'fixture license')
    const manifest = {
      fingerprint: await windowsNodeFingerprint(),
      executableSha256: await fileSha256(join(root, 'node.exe')),
      licenseSha256: await fileSha256(join(root, 'LICENSE')),
    }
    await writeFile(join(root, windowsNodeManifest), JSON.stringify(manifest))
    const destination = join(root, 'copy')
    await prepareNodeBinary({
      source: root,
      destination,
      platform: 'win32',
      architecture: 'x64',
      run: () => '{"version":"24.22.1","platform":"win32","architecture":"x64"}',
    })
    expect(await verifyWindowsNodeBuild(destination)).toEqual(manifest)
    expect(await readFile(join(destination, 'LICENSE'), 'utf8')).toBe('fixture license')
    await writeFile(join(destination, 'node.exe'), 'wrong executable')
    await expect(verifyWindowsNodeBuild(destination)).rejects.toThrow('SHA-256 mismatch')
    await writeFile(
      join(root, windowsNodeManifest),
      JSON.stringify({ ...manifest, fingerprint: 'old' }),
    )
    await expect(verifyWindowsNodeBuild(root)).rejects.toThrow('provenance')
    await rm(join(root, windowsNodeManifest))
    await expect(verifyWindowsNodeBuild(root)).rejects.toThrow()
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
