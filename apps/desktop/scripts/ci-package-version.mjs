import { execFileSync } from 'node:child_process'
import { appendFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

function isCommitSha(value) {
  return (
    typeof value === 'string' && /^(?:[a-f\d]{40}|[a-f\d]{64})$/i.test(value) && !/^0+$/.test(value)
  )
}

function committedJson(cwd, sha, path) {
  try {
    return JSON.parse(
      execFileSync('git', ['show', `${sha}:${path}`], {
        cwd,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'pipe'],
        maxBuffer: 16 * 1024 * 1024,
      }),
    )
  } catch (cause) {
    throw new Error(`Cannot read ${path} at ${sha}`, { cause })
  }
}

function versionOf(manifest, label) {
  const version = manifest?.version
  if (typeof version !== 'string' || !version.trim() || version !== version.trim()) {
    throw new Error(`${label} must have a non-empty version string without surrounding whitespace`)
  }
  return version
}

export function desktopVersionChanged({ baseSha, headSha = 'HEAD', cwd = process.cwd() }) {
  if (!isCommitSha(baseSha)) throw new Error('BASE_SHA must be the non-zero commit before the push')
  if (headSha !== 'HEAD' && !isCommitSha(headSha)) throw new Error('Invalid head commit')
  const path = 'apps/desktop/package.json'
  const previousVersion = versionOf(committedJson(cwd, baseSha, path), 'Previous package.json')
  const version = versionOf(committedJson(cwd, headSha, path), 'Current package.json')
  const lock = committedJson(cwd, headSha, 'apps/desktop/package-lock.json')
  if (
    versionOf(lock, 'package-lock.json') !== version ||
    versionOf(lock.packages?.[''], 'package-lock.json root package') !== version
  ) {
    throw new Error('package-lock.json root versions must match package.json')
  }
  return { changed: previousVersion !== version, previousVersion, version }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = desktopVersionChanged({ baseSha: process.env.BASE_SHA })
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `changed=${result.changed}\n`)
  }
  console.log(
    result.changed
      ? `Desktop version changed: ${JSON.stringify(result.previousVersion)} -> ${JSON.stringify(result.version)}; build Windows installer.`
      : `Desktop version unchanged (${JSON.stringify(result.version)}); skip Windows installer.`,
  )
}
