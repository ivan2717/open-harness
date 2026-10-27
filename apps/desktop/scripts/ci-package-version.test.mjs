import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, test } from 'vitest'
import { desktopVersionChanged } from './ci-package-version.mjs'

const directories = []
afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true })
})

function repository() {
  const cwd = mkdtempSync(join(tmpdir(), 'open-harness-package-version-'))
  directories.push(cwd)
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8' }).trim()
  git('init', '-q')
  git('config', 'user.name', 'Packaging test')
  git('config', 'user.email', 'packaging@example.invalid')
  mkdirSync(join(cwd, 'apps/desktop'), { recursive: true })
  const write = (path, value) => writeFileSync(join(cwd, path), JSON.stringify(value))
  const manifest = (version, dependencies = {}) => {
    write('apps/desktop/package.json', { name: 'desktop', version, dependencies })
    writeFileSync(join(cwd, 'apps/desktop/pnpm-lock.yaml'), "lockfileVersion: '9.0'\n")
  }
  const commit = () => {
    git('add', '.')
    git('-c', 'commit.gpgsign=false', 'commit', '-qm', 'test change')
    return git('rev-parse', 'HEAD')
  }
  manifest('0.1.0')
  const baseSha = commit()
  return { cwd, baseSha, write, manifest, commit }
}

test('dependency changes without a version change do not request packaging', () => {
  const repo = repository()
  repo.manifest('0.1.0', { react: '^19.2.0' })
  repo.commit()
  assert.deepEqual(desktopVersionChanged(repo), {
    changed: false,
    previousVersion: '0.1.0',
    version: '0.1.0',
  })
})

test('a version change requests packaging from committed contents', () => {
  const repo = repository()
  repo.manifest('0.2.0')
  const headSha = repo.commit()
  repo.manifest('0.3.0')
  assert.deepEqual(desktopVersionChanged({ ...repo, headSha }), {
    changed: true,
    previousVersion: '0.1.0',
    version: '0.2.0',
  })
})

test('a multi-commit push compares with before, rather than only the last parent', () => {
  const repo = repository()
  repo.manifest('0.2.0')
  repo.commit()
  repo.write('README.json', { description: 'a later documentation change' })
  repo.commit()
  assert.equal(desktopVersionChanged(repo).changed, true)
})

test.each([undefined, '', 'HEAD', '0'.repeat(40), 'g'.repeat(40)])(
  'invalid baseline %s is rejected rather than skipping packaging',
  (baseSha) => {
    const repo = repository()
    assert.throws(() => desktopVersionChanged({ ...repo, baseSha }), /BASE_SHA/)
  },
)

test('a missing baseline commit is an error', () => {
  const repo = repository()
  assert.throws(
    () => desktopVersionChanged({ ...repo, baseSha: '1'.repeat(40) }),
    /Cannot read apps\/desktop\/package\.json/,
  )
})

test.each([undefined, '', ' ', ' 0.2.0', 2, null])(
  'invalid current version %s is rejected',
  (version) => {
    const repo = repository()
    repo.manifest(version)
    repo.commit()
    assert.throws(() => desktopVersionChanged(repo), /Current package\.json must have/)
  },
)

test('a version-only change can retain the same pnpm dependency lock', () => {
  const repo = repository()
  const previousLock = readFileSync(join(repo.cwd, 'apps/desktop/pnpm-lock.yaml'), 'utf8')
  repo.manifest('0.2.0')
  repo.commit()
  assert.equal(readFileSync(join(repo.cwd, 'apps/desktop/pnpm-lock.yaml'), 'utf8'), previousLock)
  assert.equal(desktopVersionChanged(repo).changed, true)
})

test.each([
  ['0.1.0', false],
  ['0.2.0', true],
])('the CLI writes a boolean GitHub job output for version %s', (version, changed) => {
  const repo = repository()
  repo.manifest(version, { react: '^19.2.0' })
  repo.commit()
  const output = join(repo.cwd, 'github-output')
  const stdout = execFileSync(
    process.execPath,
    [fileURLToPath(new URL('./ci-package-version.mjs', import.meta.url))],
    {
      cwd: repo.cwd,
      encoding: 'utf8',
      env: { ...process.env, BASE_SHA: repo.baseSha, GITHUB_OUTPUT: output },
    },
  )
  assert.equal(readFileSync(output, 'utf8'), `changed=${changed}\n`)
  assert.match(stdout, changed ? /build Windows installer/ : /skip Windows installer/)
})
