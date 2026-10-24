import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, join } from 'node:path'
import { afterEach, test } from 'vitest'
import { publishWindowsRelease } from './publish-github-release.mjs'

const repository = 'owner/open-harness'
const expectedCommit = 'a'.repeat(40)
const directories = []
afterEach(() => {
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true })
})

function fixture(version = '0.1.0') {
  const directory = mkdtempSync(join(tmpdir(), 'open-harness-release-'))
  directories.push(directory)
  const installer = `Open-Harness-${version}-windows-x64-setup.exe`
  const bytes = Buffer.from('MZ verified installer fixture')
  writeFileSync(join(directory, installer), bytes)
  writeFileSync(
    join(directory, 'SHA256SUMS.txt'),
    `${createHash('sha256').update(bytes).digest('hex')}  ${installer}\n`,
  )
  const info = {
    version,
    commit: expectedCommit,
    platform: 'windows-x64',
    signed: false,
    run: `https://github.com/${repository}/actions/runs/123`,
  }
  const writeInfo = (value = info) =>
    writeFileSync(join(directory, 'build-info.json'), `\uFEFF${JSON.stringify(value)}`)
  writeInfo()
  return { directory, repository, expectedCommit, installer, info, writeInfo }
}

function missing() {
  return Object.assign(new Error('not found'), { status: 1, stderr: 'gh: Not Found (HTTP 404)' })
}

function github() {
  const state = { tag: null, release: null, assets: [], calls: [], failUpload: 0, uploads: 0 }
  const runGh = (args, options = {}) => {
    const body = options.input && JSON.parse(options.input)
    const method = args[args.indexOf('--method') + 1]
    state.calls.push({ args, method, body })
    if (args[0] === 'release') {
      assert.deepEqual(args.slice(0, 2), ['release', 'upload'])
      assert.deepEqual(args.slice(4), ['--repo', repository])
      assert.ok(state.release.draft, 'uploads must happen while the release is a draft')
      if (++state.uploads === state.failUpload) throw new Error('upload failed')
      const bytes = readFileSync(args[3])
      state.assets.push({
        name: basename(args[3]),
        state: 'uploaded',
        size: bytes.length,
        digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
      })
      return ''
    }
    assert.equal(args[0], 'api')
    assert.ok(args[1].startsWith(`repos/${repository}/`))
    const path = args[1].slice(`repos/${repository}/`.length)
    let result
    if (path.startsWith('git/ref/tags/') && method === 'GET') {
      if (!state.tag) throw missing()
      result = { object: state.tag }
    } else if (path.startsWith('git/tags/') && method === 'GET') {
      result = { object: { type: 'commit', sha: expectedCommit } }
    } else if (path === 'git/refs' && method === 'POST') {
      assert.equal(body.sha, expectedCommit)
      assert.equal(state.tag, null, 'an existing tag must never move')
      state.tag = { type: 'commit', sha: body.sha }
      result = { object: state.tag }
    } else if (path.startsWith('releases/tags/') && method === 'GET') {
      if (!state.release || state.release.draft) throw missing()
      result = state.release
    } else if (path.startsWith('releases?') && method === 'GET') {
      result = state.release ? [state.release] : []
    } else if (path === 'releases' && method === 'POST') {
      assert.equal(state.release, null)
      assert.equal(body.draft, true)
      state.release = { id: 1, ...body }
      result = state.release
    } else if (path === 'releases/1/assets?per_page=100' && method === 'GET') {
      result = state.assets
    } else if (path === 'releases/1' && method === 'GET') {
      result = state.release
    } else if (path === 'releases/1' && method === 'PATCH') {
      assert.equal(state.assets.length, 3, 'publish only after all three assets are uploaded')
      Object.assign(state.release, body)
      result = state.release
    } else {
      throw new Error(`Unexpected request: ${method} ${path}`)
    }
    return JSON.stringify(result)
  }
  return { state, runGh }
}

function draft(remote, version = '0.1.0') {
  remote.state.tag = { type: 'commit', sha: expectedCommit }
  remote.state.release = {
    id: 1,
    tag_name: `v${version}`,
    draft: true,
    target_commitish: expectedCommit,
  }
}

test('creates an exact tag and uploads all verified files before publishing a draft', () => {
  const files = fixture()
  const remote = github()
  assert.equal(
    publishWindowsRelease({ ...files, runGh: remote.runGh }),
    `https://github.com/${repository}/releases/tag/v0.1.0`,
  )
  assert.equal(remote.state.release.draft, false)
  assert.equal(remote.state.release.prerelease, false)
  assert.equal(remote.state.release.make_latest, 'legacy')
  assert.equal(remote.state.tag.sha, expectedCommit)
  assert.equal(remote.state.uploads, 3)
  assert.match(remote.state.release.body, /unsigned/)
  assert.match(remote.state.release.body, /actions\/runs\/123/)
})

test('an interrupted upload remains a draft and rerunning uploads only missing assets', () => {
  const files = fixture()
  const remote = github()
  remote.state.failUpload = 2
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }), /remains a draft/)
  assert.equal(remote.state.release.draft, true)
  assert.equal(remote.state.assets.length, 1)
  remote.state.failUpload = 0
  const start = remote.state.calls.length
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  assert.equal(remote.state.release.draft, false)
  assert.equal(
    remote.state.calls.slice(start).filter((call) => call.args[0] === 'release').length,
    2,
  )
  assert.equal(remote.state.calls.filter((call) => call.method === 'POST').length, 2)
})

test('an identical published release is a read-only success on rerun', () => {
  const files = fixture()
  const remote = github()
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  remote.state.calls = []
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  assert.ok(remote.state.calls.every((call) => call.method === 'GET'))
})

test.each([true, false])('a conflicting asset is never overwritten (draft=%s)', (isDraft) => {
  const files = fixture()
  const remote = github()
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  remote.state.release.draft = isDraft
  remote.state.assets[0].digest = `sha256:${'0'.repeat(64)}`
  remote.state.calls = []
  assert.throws(
    () => publishWindowsRelease({ ...files, runGh: remote.runGh }),
    /refusing to overwrite/,
  )
  assert.ok(remote.state.calls.every((call) => call.method === 'GET'))
})

test('a different existing tag commit fails before any write', () => {
  const files = fixture()
  const remote = github()
  remote.state.tag = { type: 'commit', sha: 'b'.repeat(40) }
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }), /different commit/)
  assert.equal(remote.state.calls.length, 1)
})

test('annotated tags are dereferenced and not recreated', () => {
  const files = fixture()
  const remote = github()
  remote.state.tag = { type: 'tag', sha: 'c'.repeat(40) }
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  assert.ok(!remote.state.calls.some((call) => call.args[1].endsWith('/git/refs')))
})

test('a draft without a tag cannot be silently redirected to another commit', () => {
  const files = fixture()
  const remote = github()
  draft(remote)
  remote.state.tag = null
  remote.state.release.target_commitish = 'main'
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }), /different commit/)
  assert.ok(remote.state.calls.every((call) => call.method === 'GET'))
})

test.each([
  ['0.2.0-beta.1', true, 'false'],
  ['0.2.0+build-123', false, 'legacy'],
])('classifies %s correctly without forcing Latest', (version, prerelease, makeLatest) => {
  const files = fixture(version)
  const remote = github()
  publishWindowsRelease({ ...files, runGh: remote.runGh })
  assert.equal(remote.state.release.prerelease, prerelease)
  assert.equal(remote.state.release.make_latest, makeLatest)
})

test.each([
  ['commit', 'b'.repeat(40)],
  ['platform', 'darwin-arm64'],
  ['signed', true],
  ['version', '../0.1.0'],
  ['version', '0.2.0-01'],
  ['run', 'https://github.com/another/repo/actions/runs/123'],
  ['run', `https://github.com/${repository}/actions/runs/123?token=secret`],
])('invalid %s metadata is rejected before contacting GitHub', (key, value) => {
  const files = fixture()
  files.writeInfo({ ...files.info, [key]: value })
  const remote = github()
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }))
  assert.equal(remote.state.calls.length, 0)
})

test('a corrupted installer and extra checksum entries are rejected before network calls', () => {
  const files = fixture()
  const remote = github()
  const original = readFileSync(join(files.directory, files.installer))
  const checksums = readFileSync(join(files.directory, 'SHA256SUMS.txt'), 'utf8')
  writeFileSync(join(files.directory, files.installer), 'corrupt')
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }), /checksum/)
  writeFileSync(join(files.directory, files.installer), original)
  writeFileSync(
    join(files.directory, 'SHA256SUMS.txt'),
    checksums + '0'.repeat(64) + '  extra.exe\n',
  )
  assert.throws(() => publishWindowsRelease({ ...files, runGh: remote.runGh }), /checksum/)
  assert.equal(remote.state.calls.length, 0)
})

test('symlinked artifacts are rejected', () => {
  const files = fixture()
  const path = join(files.directory, 'build-info.json')
  writeFileSync(join(files.directory, 'actual-info.json'), readFileSync(path))
  rmSync(path)
  symlinkSync('actual-info.json', path)
  assert.throws(() => publishWindowsRelease(files), /regular file/)
})

test.each([
  ['permission', 'secret-token (HTTP 403)', /HTTP 403/],
  ['network', 'secret-token: connection refused', /CLI error/],
])('%s errors do not look like missing releases or expose stderr', (_, stderr, pattern) => {
  const files = fixture()
  const runGh = () => {
    throw Object.assign(new Error('failed'), { status: 1, stderr })
  }
  assert.throws(
    () => publishWindowsRelease({ ...files, runGh }),
    (error) => {
      assert.match(error.message, pattern)
      assert.doesNotMatch(error.message, /secret-token/)
      return true
    },
  )
})

test('a zero expected commit is rejected before any GitHub operation', () => {
  const files = fixture()
  const remote = github()
  assert.throws(
    () => publishWindowsRelease({ ...files, expectedCommit: '0'.repeat(40), runGh: remote.runGh }),
    /non-zero commit SHA/,
  )
  assert.equal(remote.state.calls.length, 0)
})
