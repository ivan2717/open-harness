import { execFileSync } from 'node:child_process'
import { cp, mkdir, realpath, rm, stat } from 'node:fs/promises'
import { isAbsolute, join, relative, sep } from 'node:path'

export const nodeRelease = '24.22.1'
// https://nodejs.org/dist/v24.22.1/SHASUMS256.txt
const targets = {
  'darwin-arm64': [
    'darwin-arm64',
    'tar.gz',
    '89c682593d76cd44233fa53b1563264e7de9cbb189cad169135c5b4fadfbc0a7',
  ],
  'darwin-x64': [
    'darwin-x64',
    'tar.gz',
    'd132557e7ee2b85af08b0c8c34338e65c6430f2238d01f727728ef42dc83b035',
  ],
  'win32-x64': [
    'win-x64',
    'zip',
    'd2046d28c2c3dcd211ac5145c0bf30e2047ef9dfb306693c8c6e2eccd8b95fa5',
  ],
  'linux-x64': [
    'linux-x64',
    'tar.gz',
    'eb6728e77c576a35162c122c45ccadf224d89ff00abc8dae46cc2897c6002d7c',
  ],
  'linux-arm64': [
    'linux-arm64',
    'tar.gz',
    '33c37be234ba8bee041961047b19160f4d7d0f164e8e246f78bdd10feff148b3',
  ],
}

export function nodeExecutable(directory, platform = process.platform) {
  return join(directory, ...(platform === 'win32' ? ['node.exe'] : ['bin', 'node']))
}

export function nodeDownload(platform, architecture) {
  const target = targets[`${platform}-${architecture}`]
  if (!target)
    throw new Error(
      `No managed Node build for ${platform}-${architecture}. Supply OPENHARNESS_NODE_RUNTIME_DIR explicitly.`,
    )
  const prefix = `node-v${nodeRelease}-${target[0]}`
  const filename = `${prefix}.${target[1]}`
  return {
    prefix,
    filename,
    sha256: target[2],
    url: `https://nodejs.org/dist/v${nodeRelease}/${filename}`,
  }
}

export function validateNodeArchiveEntries(listing, prefix) {
  for (const entry of listing.split(/\r?\n/).filter(Boolean)) {
    if (!entry.startsWith(`${prefix}/`) || entry.includes('\\') || entry.split('/').includes('..'))
      throw new Error('Unexpected path in Node runtime archive')
  }
}

export function verifyNodeBinary(directory, platform, architecture, probe = execFileSync) {
  const output = probe(
    nodeExecutable(directory, platform),
    [
      '-e',
      'console.log(JSON.stringify({version:process.versions.node,platform:process.platform,architecture:process.arch}))',
    ],
    {
      encoding: 'utf8',
      timeout: 10_000,
      windowsHide: true,
      env: { ...process.env, NODE_OPTIONS: '', NODE_PATH: '', ELECTRON_RUN_AS_NODE: '' },
    },
  )
  const metadata = JSON.parse(output)
  if (
    !/^\d+\.\d+\.\d+$/.test(metadata.version) ||
    Number(metadata.version.split('.')[0]) < 22 ||
    metadata.platform !== platform ||
    metadata.architecture !== architecture
  )
    throw new Error(
      'Node runtime payload does not match the supported version, build platform and architecture',
    )
  return metadata
}

/** Copy only the independent executable and license; pnpm is our package manager. */
export async function prepareNodeBinary({
  cache,
  destination,
  source,
  platform,
  architecture,
  run = execFileSync,
  downloadFile,
}) {
  let root = source
  let extracted
  try {
    if (!source) {
      const artifact = nodeDownload(platform, architecture)
      const archive = await downloadFile({
        ...artifact,
        destination: join(cache, artifact.filename),
      })
      validateNodeArchiveEntries(
        run('tar', ['-tf', archive], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 }),
        artifact.prefix,
      )
      extracted = `${destination}-download`
      await mkdir(extracted, { recursive: true })
      const executable = platform === 'win32' ? 'node.exe' : 'bin/node'
      run(
        'tar',
        [
          '-xf',
          archive,
          '-C',
          extracted,
          `${artifact.prefix}/${executable}`,
          `${artifact.prefix}/LICENSE`,
        ],
        { stdio: 'inherit' },
      )
      root = join(extracted, artifact.prefix)
    } else if (!isAbsolute(source)) throw new Error('OPENHARNESS_NODE_RUNTIME_DIR must be absolute')
    root = await realpath(root)
    const executable = await realpath(nodeExecutable(root, platform))
    const license = await realpath(join(root, 'LICENSE'))
    const location = relative(root, executable)
    const licenseLocation = relative(root, license)
    if (
      isAbsolute(location) ||
      location === '..' ||
      location.startsWith(`..${sep}`) ||
      !(await stat(executable)).isFile()
    )
      throw new Error('Node runtime executable must be a regular file within the supplied payload')
    if (
      isAbsolute(licenseLocation) ||
      licenseLocation === '..' ||
      licenseLocation.startsWith(`..${sep}`) ||
      !(await stat(license)).isFile()
    )
      throw new Error('Node runtime license must be a regular file within the supplied payload')
    await mkdir(platform === 'win32' ? destination : join(destination, 'bin'), { recursive: true })
    await cp(executable, nodeExecutable(destination, platform))
    await cp(license, join(destination, 'LICENSE'))
    const metadata = verifyNodeBinary(destination, platform, architecture, run)
    if (!source && metadata.version !== nodeRelease)
      throw new Error('Downloaded Node version does not match the pinned release')
    return metadata
  } finally {
    if (extracted) await rm(extracted, { recursive: true, force: true })
  }
}
