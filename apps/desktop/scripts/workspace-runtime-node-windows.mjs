import { execFileSync } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { cp, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDirectory = dirname(fileURLToPath(import.meta.url))
const patch = join(scriptDirectory, 'workspace-runtime/patches/libuv-appcontainer-pipes.patch')
export const windowsNodeBuild = {
  version: '24.22.1',
  sourceSha256: '001773d9bd53fbd27a25efd42aa6a9693ebfe500998ca9bec635e9b6ee79bc91',
  upstreamCommit: '2cadaa40167050baf7c6905ac897e6fb57afb2c6',
  patchSha256: 'e9020b399c1a586574f2e165f455185fa18b5e5cda20f4774a570d672ac6c05d',
  beforeSha256: 'a712502b4bc71ad994e1c639fcba386ac1748ab8dcb693038d0203dbdbfe107a',
  afterSha256: '6375fa10b714e4e0e0b76fa58e33e936e1ad3d70ddf5b44b325baf2ee20aafb0',
  // Default configuration is Release. The explicit `release` alias also builds
  // cctest and enables LTCG; neither is needed for the shipped executable.
  arguments: ['x64', 'vs2022', 'nonpm', 'nocorepack', 'no-cctest', 'full-icu'],
}
export const windowsNodeManifest = 'openharness-node-build.json'

export async function fileSha256(path) {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  return hash.digest('hex')
}

export async function windowsNodeFingerprint() {
  if ((await fileSha256(patch)) !== windowsNodeBuild.patchSha256)
    throw new Error('Node AppContainer patch SHA-256 mismatch')
  return createHash('sha256')
    .update(JSON.stringify(windowsNodeBuild))
    .update(await readFile(fileURLToPath(import.meta.url)))
    .digest('hex')
}

/** A cache marker is not sufficient: verify the executable and its provenance. */
export async function verifyWindowsNodeBuild(directory) {
  const manifest = JSON.parse(await readFile(join(directory, windowsNodeManifest), 'utf8'))
  if (
    manifest.fingerprint !== (await windowsNodeFingerprint()) ||
    manifest.executableSha256 !== (await fileSha256(join(directory, 'node.exe'))) ||
    manifest.licenseSha256 !== (await fileSha256(join(directory, 'LICENSE')))
  )
    throw new Error('Windows Node build provenance or payload SHA-256 mismatch')
  return manifest
}

export async function applyWindowsNodePatch(source, run = execFileSync) {
  await windowsNodeFingerprint()
  const target = join(source, 'deps/uv/src/win/pipe.c')
  if ((await fileSha256(target)) !== windowsNodeBuild.beforeSha256)
    throw new Error('Unexpected libuv source; refusing to apply the Node AppContainer backport')
  // Git for Windows commonly enables autocrlf globally. Keep the official
  // archive's LF bytes, otherwise applying a patch changes the entire file.
  const gitOptions = ['-c', 'core.autocrlf=false', '-c', 'core.eol=lf', 'apply']
  run('git', [...gitOptions, '--check', patch], { cwd: source, stdio: 'inherit' })
  run('git', [...gitOptions, patch], { cwd: source, stdio: 'inherit' })
  if ((await fileSha256(target)) !== windowsNodeBuild.afterSha256)
    throw new Error('Patched libuv source SHA-256 mismatch')
}

/** Temporary upstream backport until an official Node release includes libuv #5181. */
export async function prepareWindowsNode({ cache, downloadFile, run = execFileSync }) {
  if (process.platform !== 'win32' || process.arch !== 'x64')
    throw new Error('Build the AppContainer-compatible Node runtime on Windows x64')
  const fingerprint = await windowsNodeFingerprint()
  const ready = resolve(cache, `node-windows-${fingerprint}`)
  try {
    await verifyWindowsNodeBuild(ready)
    return ready
  } catch {
    // Missing or damaged caches are rebuilt from verified source.
  }
  const prefix = `node-v${windowsNodeBuild.version}`
  const archive = await downloadFile({
    url: `https://nodejs.org/dist/v${windowsNodeBuild.version}/${prefix}.tar.gz`,
    sha256: windowsNodeBuild.sourceSha256,
    destination: join(cache, `${prefix}.tar.gz`),
  })
  const listing = run('tar', ['-tf', archive], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 })
  for (const entry of listing.split(/\r?\n/).filter(Boolean)) {
    if (!entry.startsWith(`${prefix}/`) || entry.includes('\\') || entry.split('/').includes('..'))
      throw new Error('Unexpected path in Node source archive')
  }
  const staging = resolve(cache, `.node-build-${randomUUID()}`)
  if (/\s/.test(staging)) throw new Error('Node source build cache path must not contain spaces')
  await mkdir(staging, { recursive: true })
  try {
    run('tar', ['-xf', archive, '-C', staging], { stdio: 'inherit' })
    const source = join(staging, prefix)
    await applyWindowsNodePatch(source, run)
    // vcbuild locates the supported VS/Clang toolchain and Python itself.
    // All arguments are fixed, not input from a model or the app renderer.
    run('cmd.exe', ['/d', '/s', '/c', `vcbuild.bat ${windowsNodeBuild.arguments.join(' ')}`], {
      cwd: source,
      stdio: 'inherit',
      timeout: 90 * 60_000,
      windowsHide: true,
    })
    const payload = join(staging, 'payload')
    await mkdir(payload)
    await cp(join(source, 'Release/node.exe'), join(payload, 'node.exe'))
    await cp(join(source, 'LICENSE'), join(payload, 'LICENSE'))
    const manifest = {
      ...windowsNodeBuild,
      fingerprint,
      executableSha256: await fileSha256(join(payload, 'node.exe')),
      licenseSha256: await fileSha256(join(payload, 'LICENSE')),
    }
    await writeFile(join(payload, windowsNodeManifest), `${JSON.stringify(manifest, null, 2)}\n`)
    await verifyWindowsNodeBuild(payload)
    await rm(ready, { recursive: true, force: true })
    await rename(payload, ready)
    return ready
  } finally {
    await rm(staging, { recursive: true, force: true })
  }
}
