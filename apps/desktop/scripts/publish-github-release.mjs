import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { appendFileSync, lstatSync, readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const semver =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][\dA-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][\dA-Za-z-]*))*))?(?:\+[\dA-Za-z-]+(?:\.[\dA-Za-z-]+)*)?$/

function regularFile(directory, name) {
  const path = join(directory, name)
  if (!lstatSync(path).isFile()) throw new Error(`${name} must be a regular file`)
  const bytes = readFileSync(path)
  return {
    path,
    name,
    size: bytes.length,
    digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
    bytes,
  }
}

function artifact(directory, repository, expectedCommit) {
  const infoFile = regularFile(directory, 'build-info.json')
  const info = JSON.parse(infoFile.bytes.toString('utf8').replace(/^\uFEFF/, ''))
  const version = typeof info.version === 'string' && semver.exec(info.version)
  if (!version || info.version.length > 128) throw new Error('Invalid desktop SemVer version')
  if (info.commit !== expectedCommit)
    throw new Error('Artifact commit does not match expected commit')
  if (info.platform !== 'windows-x64' || info.signed !== false) {
    throw new Error('Expected an unsigned Windows x64 build')
  }
  const run = new URL(info.run)
  if (
    run.origin !== 'https://github.com' ||
    run.username ||
    run.password ||
    run.search ||
    run.hash ||
    !run.pathname.startsWith(`/${repository}/actions/runs/`) ||
    !/^[1-9]\d*$/.test(run.pathname.slice(`/${repository}/actions/runs/`.length))
  ) {
    throw new Error('Build workflow URL must belong to the release repository')
  }
  const installer = regularFile(directory, `Open-Harness-${info.version}-windows-x64-setup.exe`)
  const checksums = regularFile(directory, 'SHA256SUMS.txt')
  const entry = checksums.bytes
    .toString('utf8')
    .replace(/^\uFEFF/, '')
    .trim()
  const match = /^([a-f\d]{64})[ \t]+\*?([^\r\n]+)$/i.exec(entry)
  if (
    !match ||
    match[2] !== installer.name ||
    `sha256:${match[1].toLowerCase()}` !== installer.digest
  ) {
    throw new Error('Installer checksum does not match the single SHA256SUMS entry')
  }
  return {
    info,
    prerelease: Boolean(version[4]),
    files: [installer, checksums, infoFile].map(({ bytes: _, ...file }) => file),
  }
}

function defaultRunGh(args, options = {}) {
  return execFileSync('gh', args, {
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  })
}

export function publishWindowsRelease({
  directory,
  repository,
  expectedCommit,
  runGh = defaultRunGh,
}) {
  if (!/^[A-Za-z\d][\w.-]*\/[A-Za-z\d][\w.-]*$/.test(repository ?? '')) {
    throw new Error('GITHUB_REPOSITORY must be owner/repository')
  }
  if (!/^[a-f\d]{40}$/i.test(expectedCommit ?? '') || /^0+$/.test(expectedCommit)) {
    throw new Error('Expected a full non-zero commit SHA')
  }
  const { info, prerelease, files } = artifact(resolve(directory), repository, expectedCommit)
  const tag = `v${info.version}`
  const root = `repos/${repository}`
  const url = `https://github.com/${repository}/releases/tag/${encodeURIComponent(tag)}`
  const api = (endpoint, method = 'GET', body, optional = false) => {
    const args = ['api', `${root}/${endpoint}`, '--method', method]
    if (body !== undefined) args.push('--input', '-')
    let result
    try {
      result = runGh(args, body === undefined ? {} : { input: JSON.stringify(body) })
    } catch (error) {
      const status = /\(HTTP (\d{3})\)/.exec(String(error.stderr))?.[1]
      if (optional && status === '404') return null
      // Do not print the command's environment, headers, or raw authentication errors.
      throw new Error(
        `GitHub ${method} ${endpoint} failed (${status ? `HTTP ${status}` : 'CLI error'})`,
      )
    }
    return JSON.parse(result)
  }
  const tagCommit = () => {
    let object = api(`git/ref/tags/${encodeURIComponent(tag)}`, 'GET', undefined, true)?.object
    if (!object) return null
    for (let depth = 0; depth < 8 && object.type === 'tag'; depth++) {
      object = api(`git/tags/${object.sha}`).object
    }
    if (object.type !== 'commit' || object.sha !== expectedCommit) {
      throw new Error(`${tag} already points to a different commit or unsupported object`)
    }
    return object.sha
  }
  const findRelease = () => {
    const published = api(`releases/tags/${encodeURIComponent(tag)}`, 'GET', undefined, true)
    if (published) return published
    // The tag endpoint documents published releases only; drafts require listing.
    for (let page = 1; ; page++) {
      const releases = api(`releases?per_page=100&page=${page}`)
      const matches = releases.filter((release) => release.tag_name === tag)
      if (matches.length > 1) throw new Error(`Multiple draft releases use ${tag}`)
      if (matches.length) return matches[0]
      if (releases.length < 100) return null
    }
  }
  const assertAssets = (assets, allowMissing) => {
    const missing = []
    for (const file of files) {
      const matches = assets.filter((asset) => asset.name === file.name)
      if (matches.length === 0 && allowMissing) {
        missing.push(file)
      } else if (
        matches.length !== 1 ||
        matches[0].state !== 'uploaded' ||
        matches[0].size !== file.size ||
        matches[0].digest !== file.digest
      ) {
        throw new Error(
          `Release asset is missing or differs: ${file.name}; refusing to overwrite it`,
        )
      }
    }
    return missing
  }

  let commit = tagCommit()
  let release = findRelease()
  if (release && (!Number.isSafeInteger(release.id) || release.tag_name !== tag)) {
    throw new Error('Unexpected release identity')
  }
  if (release && !release.draft) {
    if (!commit) throw new Error('Published release has no matching tag')
    assertAssets(api(`releases/${release.id}/assets?per_page=100`), false)
    return url
  }
  if (!commit) {
    if (release && release.target_commitish !== expectedCommit) {
      throw new Error('Draft release targets a different commit')
    }
    api('git/refs', 'POST', { ref: `refs/tags/${tag}`, sha: expectedCommit })
    commit = tagCommit()
    if (!commit) throw new Error('Created tag could not be verified')
  }
  if (!release) {
    release = api('releases', 'POST', {
      tag_name: tag,
      target_commitish: expectedCommit,
      name: `Open Harness ${tag}`,
      draft: true,
      prerelease,
      make_latest: 'false',
      body: [
        'Windows x64 installer. This build is unsigned; Windows may show an unknown-publisher prompt.',
        '',
        `Source commit: [${expectedCommit}](https://github.com/${repository}/commit/${expectedCommit})`,
        `Validated build: ${info.run}`,
        '',
        'Download the setup.exe below. SHA256SUMS.txt verifies the installer; build-info.json records its source and build.',
        `See [Windows feature support](https://github.com/${repository}/blob/${expectedCommit}/apps/desktop/README.md#windows-ci-installer) for current platform limits, including computer and browser control.`,
      ].join('\n'),
    })
  }
  if (!Number.isSafeInteger(release.id) || release.tag_name !== tag || !release.draft) {
    throw new Error('Expected an unpublished release with the requested tag')
  }
  const missing = assertAssets(api(`releases/${release.id}/assets?per_page=100`), true)
  for (const file of missing) {
    try {
      runGh(['release', 'upload', tag, file.path, '--repo', repository])
    } catch {
      throw new Error(
        `Uploading ${file.name} failed; the release remains a draft and can be retried`,
      )
    }
  }
  assertAssets(api(`releases/${release.id}/assets?per_page=100`), false)
  if (!tagCommit()) throw new Error('Release tag disappeared before publishing')
  const current = api(`releases/${release.id}`)
  if (current.tag_name !== tag) throw new Error('Release tag changed before publishing')
  if (current.draft) {
    const published = api(`releases/${release.id}`, 'PATCH', {
      draft: false,
      prerelease,
      make_latest: prerelease ? 'false' : 'legacy',
    })
    if (published.draft || published.tag_name !== tag) throw new Error('Release was not published')
  }
  if (!tagCommit()) throw new Error('Published release has no matching tag')
  return url
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const url = publishWindowsRelease({
    directory: process.argv[2],
    repository: process.env.GITHUB_REPOSITORY,
    expectedCommit: process.env.GITHUB_SHA,
  })
  console.log(url)
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(process.env.GITHUB_STEP_SUMMARY, `Windows download: [GitHub Release](${url})\n`)
  }
}
