import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export function directoryBuildArguments(platform) {
  return [
    '--dir',
    ...(platform === 'darwin'
      ? [
          '-c.mac.identity=-',
          '-c.mac.sign=./scripts/sign-local-mac.mjs',
          '-c.directories.output=release-next',
        ]
      : []),
  ]
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const require = createRequire(import.meta.url)
  const result = spawnSync(
    process.execPath,
    [
      require.resolve('electron-builder/cli.js'),
      ...directoryBuildArguments(process.platform),
      ...process.argv.slice(2),
    ],
    { stdio: 'inherit' },
  )
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
}
