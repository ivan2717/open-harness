import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'vitest'
import { withLocalSigning } from './local-mac-signing.mjs'
import { archiveMacApp } from './archive-mac-app.mjs'

const run = (program, args) =>
  execFileSync(program, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] })
const mac = process.platform === 'darwin'

test.skipIf(!mac)(
  'changed binaries keep their certificate requirement across builds, but unrelated code is rejected',
  async () => {
    const root = await mkdtemp(join(tmpdir(), 'oh-signing-test-'))
    const searchList = run('/usr/bin/security', ['list-keychains', '-d', 'user'])
    const certificateRoot = join(root, 'signing')
    let requirement
    let certificate
    try {
      for (const version of [1, 2]) {
        await withLocalSigning(async ({ keychain, fingerprint }) => {
          const source = join(root, 'main.c')
          const binary = join(root, `version-${version}`)
          await writeFile(source, `int main(void) { return ${version}; }\n`)
          run('xcrun', ['clang', source, '-o', binary])
          run('/usr/bin/codesign', [
            '--force',
            '--sign',
            fingerprint,
            '--keychain',
            keychain,
            '--identifier',
            'dev.openharness.signing-test',
            '--timestamp=none',
            binary,
          ])
          const current = run('/usr/bin/codesign', ['-d', '-r-', binary]).trim()
          if (version === 1) {
            requirement = current.replace(/^designated => /, '')
            certificate = await readFile(join(certificateRoot, 'identity', 'certificate.pem'))
            assert.match(
              requirement,
              /identifier "dev.openharness.signing-test" and certificate leaf = H"[a-f0-9]{40}"/,
            )
          }
          run('/usr/bin/codesign', ['--verify', '--strict', '-R', `=${requirement}`, binary])
          assert.deepEqual(
            await readFile(join(certificateRoot, 'identity', 'certificate.pem')),
            certificate,
          )
        }, certificateRoot)
        assert.equal(run('/usr/bin/security', ['list-keychains', '-d', 'user']), searchList)
      }
      const unsigned = join(root, 'unrelated')
      run('xcrun', ['clang', join(root, 'main.c'), '-o', unsigned])
      run('/usr/bin/codesign', [
        '--force',
        '--sign',
        '-',
        '--identifier',
        'dev.openharness.signing-test',
        unsigned,
      ])
      assert.throws(() => run('/usr/bin/codesign', ['--verify', '-R', `=${requirement}`, unsigned]))
      await assert.rejects(
        withLocalSigning(() => {
          throw new Error('sign failed')
        }, certificateRoot),
        /sign failed/,
      )
      assert.equal(run('/usr/bin/security', ['list-keychains', '-d', 'user']), searchList)
      assert.equal((await stat(join(certificateRoot, 'identity', 'password'))).mode & 0o777, 0o600)
      await rm(join(certificateRoot, 'identity', 'password'))
      await assert.rejects(
        withLocalSigning(
          () => assert.fail('must not replace an incomplete identity'),
          certificateRoot,
        ),
      )
      assert.deepEqual(
        await readFile(join(certificateRoot, 'identity', 'certificate.pem')),
        certificate,
      )
    } finally {
      run('/usr/bin/security', [
        'delete-keychain',
        join(certificateRoot, 'identity', 'identity.keychain-db'),
      ])
      await rm(root, { recursive: true, force: true })
    }
  },
  30_000,
)

test.skipIf(!mac)(
  'rollback archives retain bundle contents and reject unrelated applications',
  async () => {
    const root = await mkdtemp(join(tmpdir(), 'oh-archive-test-'))
    try {
      const { mkdir } = await import('node:fs/promises')
      const app = join(root, 'previous.app')
      await mkdir(join(app, 'Contents'), { recursive: true })
      const plist = (id) =>
        `<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>${id}</string></dict></plist>`
      await writeFile(join(app, 'Contents', 'Info.plist'), plist('other.app'))
      await assert.rejects(archiveMacApp(app), /Only Open Harness/)
      await writeFile(join(app, 'Contents', 'Info.plist'), plist('dev.openharness.desktop'))
      const archive = await archiveMacApp(app)
      await assert.rejects(stat(app), { code: 'ENOENT' })
      assert.equal(
        run('/usr/bin/unzip', ['-p', archive, 'previous.app/Contents/Info.plist']),
        plist('dev.openharness.desktop'),
      )
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  },
)
