import { execFileSync } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { createReadStream, createWriteStream } from 'node:fs'
import {
  access,
  cp,
  mkdir,
  readFile,
  readdir,
  readlink,
  realpath,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { Readable } from 'node:stream'
import { fileURLToPath } from 'node:url'
import { nodeDownload, nodeExecutable, prepareNodeBinary } from './workspace-runtime-node.mjs'

const configuration = join(dirname(fileURLToPath(import.meta.url)), 'workspace-runtime')
export const pythonRelease = { version: '3.12.15', release: '20261021' }
const pythonTargets = {
  'darwin-arm64': [
    'aarch64-apple-darwin',
    '7b8d00b751db21cea87dc551a8f8165147c5fbc5251d5537d7e5497dde00a219',
  ],
  'darwin-x64': [
    'x86_64-apple-darwin',
    'f12e0504397683d6be0745ffd4c39ef45a455cec169adf35c628840d06133773',
  ],
  'win32-x64': [
    'x86_64-pc-windows-msvc',
    'aac5d331fc3307343f95928fdbea4583f0d53973d08c7e1d081884fbd229a0e1',
  ],
  'linux-x64': [
    'x86_64-unknown-linux-gnu',
    '6d7521138fd33afddc1ccd08abb007d9a2829e38117c2576f9efdc0ec73ce772',
  ],
  'linux-arm64': [
    'aarch64-unknown-linux-gnu',
    'b4567aa5a11f51b540c4dfd9e082ebf23e554f8ac17b85caa6cea23e83693394',
  ],
}

export function pythonDownload(platform, architecture) {
  const target = pythonTargets[`${platform}-${architecture}`]
  if (!target)
    throw new Error(
      `No managed Python build for ${platform}-${architecture}. Supply OPENHARNESS_PYTHON_RUNTIME_DIR explicitly.`,
    )
  const filename = `cpython-${pythonRelease.version}+${pythonRelease.release}-${target[0]}-install_only_stripped.tar.gz`
  return {
    filename,
    sha256: target[1],
    url: `https://github.com/astral-sh/python-build-standalone/releases/download/${pythonRelease.release}/${encodeURIComponent(filename)}`,
  }
}

export async function verifyPayloadLinks(root, current = root) {
  for (const entry of await readdir(current, { withFileTypes: true })) {
    const path = join(current, entry.name)
    if (entry.isSymbolicLink()) {
      const target = relative(root, await realpath(path))
      if (
        isAbsolute(await readlink(path)) ||
        isAbsolute(target) ||
        target === '..' ||
        target.startsWith(`..${sep}`)
      )
        throw new Error('Runtime payload contains an escaping symbolic link')
    } else if (entry.isDirectory()) await verifyPayloadLinks(root, path)
    else if (!entry.isFile()) throw new Error('Runtime payload contains a non-regular file')
  }
}

async function sha256(file) {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(file)) hash.update(chunk)
  return hash.digest('hex')
}

export async function verifiedDownload({ url, sha256: expected, destination, fetchFile = fetch }) {
  if (
    await sha256(destination).then(
      (actual) => actual === expected,
      () => false,
    )
  )
    return destination
  const temporary = `${destination}.${randomUUID()}.tmp`
  await mkdir(dirname(destination), { recursive: true })
  try {
    const response = await fetchFile(url, { signal: AbortSignal.timeout(120_000) })
    if (!response.ok || !response.body)
      throw new Error(`Runtime download failed: HTTP ${response.status}`)
    await pipeline(Readable.fromWeb(response.body), createWriteStream(temporary, { flags: 'wx' }))
    if ((await sha256(temporary)) !== expected)
      throw new Error('Runtime archive SHA-256 mismatch; refusing to execute it')
    await rm(destination, { force: true })
    await rename(temporary, destination)
    return destination
  } finally {
    await rm(temporary, { force: true })
  }
}

export function validateArchiveEntries(listing) {
  for (const entry of listing.split(/\r?\n/).filter(Boolean)) {
    if (!entry.startsWith('python/') || entry.includes('\\') || entry.split('/').includes('..'))
      throw new Error('Unexpected path in Python runtime archive')
  }
}

async function pnpmScript(environment = process.env) {
  const candidates = [
    environment.npm_execpath,
    resolve(dirname(process.execPath), '../lib/node_modules/pnpm/bin/pnpm.cjs'),
    join(dirname(process.execPath), 'node_modules/pnpm/bin/pnpm.cjs'),
  ]
  for (const candidate of candidates) {
    if (
      candidate &&
      /[\\/]pnpm\.cjs$/.test(candidate) &&
      (await access(candidate).then(
        () => true,
        () => false,
      ))
    )
      return candidate
  }
  throw new Error('Build-time pnpm was not found. Run this build using pnpm run build.')
}

function verifyManagedNodeRuntime(directory, platform, probe) {
  probe(
    nodeExecutable(join(directory, 'node'), platform),
    [
      '-e',
      'if(Number(process.versions.node.split(".")[0])<22||process.versions.electron)throw new Error("Expected standalone Node 22+"); const r=require("node:module").createRequire(process.argv[1]); for(const name of ["csv-parse","csv-stringify","pngjs"]) r(name); require("node:fs").accessSync(require("node:path").join(require("node:path").dirname(process.argv[1]),"node_modules/pnpm/bin/pnpm.cjs"))',
      join(directory, 'node', 'package.json'),
    ],
    {
      timeout: 10_000,
      windowsHide: true,
      env: { ...process.env, NODE_OPTIONS: '', NODE_PATH: '', ELECTRON_RUN_AS_NODE: '' },
    },
  )
}

export function verifyManagedRuntime(directory, platform = process.platform, probe = execFileSync) {
  const pythonExecutable = join(
    directory,
    'python',
    ...(platform === 'win32' ? ['python.exe'] : ['bin', 'python3']),
  )
  probe(
    pythonExecutable,
    [
      '-I',
      '-B',
      '-c',
      `import os,tempfile,io
with tempfile.TemporaryDirectory(prefix="openharness-runtime-probe-") as temporary:
 os.environ["MPLCONFIGDIR"]=temporary
 os.environ["XDG_CACHE_HOME"]=temporary
 import PIL,docx,pptx,openpyxl,pypdf,reportlab,pip,venv,numpy,pandas,matplotlib,ssl,sqlite3
 from lxml import etree
 from PIL import Image
 matplotlib.use("Agg")
 import matplotlib.pyplot as plt
 frame=pandas.DataFrame({"value":numpy.array([1,2,3])})
 assert pandas.read_csv(io.StringIO(frame.to_csv(index=False)))["value"].sum()==6
 figure,axes=plt.subplots()
 axes.plot(frame["value"])
 image_path=os.path.join(temporary,"probe.png")
 figure.savefig(image_path)
 plt.close(figure)
 with Image.open(image_path) as image: image.verify()`,
    ],
    {
      // First imports load many native extensions. Antivirus/code-signature
      // checks after relocation can take longer than a warmed cache import.
      timeout: 120_000,
      windowsHide: true,
      env: { ...process.env, PYTHONHOME: '', PYTHONPATH: '' },
    },
  )
  verifyManagedNodeRuntime(directory, platform, probe)
}

/** A completion marker alone cannot validate a partly deleted or broken cache. */
export async function reusableRuntime(
  directory,
  fingerprint,
  platform,
  probe = execFileSync,
  nodeOnly = false,
) {
  if ((await readFile(join(directory, '.ready'), 'utf8').catch(() => '')) !== fingerprint)
    return false
  try {
    await verifyPayloadLinks(await realpath(join(directory, 'node')))
    if (!nodeOnly) await verifyPayloadLinks(await realpath(join(directory, 'python')))
    if (nodeOnly) verifyManagedNodeRuntime(directory, platform, probe)
    else verifyManagedRuntime(directory, platform, probe)
    return true
  } catch {
    return false
  }
}

/** The app invokes pnpm.cjs directly; generated shims contain build-machine paths. */
export async function cleanManagedNodeMetadata(nodeDirectory) {
  await rm(join(nodeDirectory, 'node_modules', '.modules.yaml'), { force: true })
  await rm(join(nodeDirectory, 'node_modules', '.bin'), { recursive: true, force: true })
}

/** Network installs happen only in the build cache; the shipped app is offline-ready. */
export async function prepareManagedRuntime({
  cache,
  platform = process.platform,
  architecture = process.arch,
  source,
  nodeSource,
  run = execFileSync,
}) {
  const download = source ? null : pythonDownload(platform, architecture)
  const nodeArtifact = nodeSource ? null : nodeDownload(platform, architecture)
  const requirements = await readFile(join(configuration, 'requirements.txt'), 'utf8')
  const packageLock = await readFile(join(configuration, 'node/pnpm-lock.yaml'), 'utf8')
  const fingerprint = createHash('sha256')
    .update(JSON.stringify({ format: 7, download, nodeArtifact, platform, architecture }))
    .update(requirements)
    .update(packageLock)
    .digest('hex')
  const ready = join(cache, fingerprint)
  const python = source ?? join(ready, 'python')
  const node = join(ready, 'node')
  // Offline overrides cache only Node dependencies; the supplied Python is
  // independently checked after relocation on every build.
  const reusable =
    !nodeSource && (await reusableRuntime(ready, fingerprint, platform, run, Boolean(source)))
  if (reusable) return { python, node, fingerprint }
  await rm(ready, { recursive: true, force: true })
  await mkdir(cache, { recursive: true })
  const staging = join(cache, `.build-${randomUUID()}`)
  await mkdir(staging)
  try {
    if (download) {
      const archive = await verifiedDownload({
        ...download,
        destination: join(cache, download.filename),
      })
      const listing = run('tar', ['-tzf', archive], {
        encoding: 'utf8',
        maxBuffer: 4 * 1024 * 1024,
      })
      validateArchiveEntries(listing)
      run('tar', ['-xzf', archive, '-C', staging], { stdio: 'inherit' })
      const executable = join(
        staging,
        'python',
        ...(platform === 'win32' ? ['python.exe'] : ['bin', 'python3']),
      )
      run(
        executable,
        [
          '-I',
          '-m',
          'pip',
          '--isolated',
          'install',
          '--disable-pip-version-check',
          '--require-hashes',
          '--only-binary=:all:',
          '--no-compile',
          '--index-url',
          'https://pypi.org/simple',
          '--cache-dir',
          join(cache, 'pip'),
          '-r',
          join(configuration, 'requirements.txt'),
        ],
        {
          stdio: 'inherit',
          timeout: 600_000,
          windowsHide: true,
          env: { ...process.env, PYTHONHOME: '', PYTHONPATH: '' },
        },
      )
      const standardLibrary = run(
        executable,
        ['-I', '-c', 'import sysconfig; print(sysconfig.get_path("stdlib"))'],
        { encoding: 'utf8' },
      ).trim()
      await writeFile(
        join(standardLibrary, 'EXTERNALLY-MANAGED'),
        '[externally-managed]\nError=This Python belongs to Open Harness. Install additional packages in a task virtual environment.\n',
      )
    }
    const nodeStage = join(staging, 'node')
    await mkdir(nodeStage)
    await prepareNodeBinary({
      cache,
      destination: nodeStage,
      source: nodeSource,
      platform,
      architecture,
      run,
      downloadFile: verifiedDownload,
    })
    for (const file of ['package.json', 'pnpm-lock.yaml'])
      await cp(join(configuration, 'node', file), join(nodeStage, file))
    const pnpmCache = join(cache, 'pnpm')
    const pnpmHome = join(pnpmCache, 'home')
    const userConfig = join(pnpmCache, 'empty-config')
    await mkdir(pnpmHome, { recursive: true })
    await writeFile(userConfig, '')
    run(
      process.execPath,
      [
        await pnpmScript(),
        '--dir',
        nodeStage,
        '--store-dir',
        join(pnpmCache, 'store'),
        '--ignore-workspace',
        '--ignore-pnpmfile',
        'install',
        '--frozen-lockfile',
        '--ignore-scripts',
        // A flat copy avoids Windows pnpm junctions pointing back into the
        // build cache after the application resources have been relocated.
        '--config.node-linker=hoisted',
        '--config.package-import-method=copy',
        '--config.manage-package-manager-versions=false',
        '--config.package-manager-strict-version=true',
        '--reporter=append-only',
      ],
      {
        stdio: 'inherit',
        timeout: 300_000,
        windowsHide: true,
        env: {
          ...process.env,
          HOME: pnpmHome,
          USERPROFILE: pnpmHome,
          PNPM_HOME: pnpmHome,
          XDG_CACHE_HOME: join(pnpmCache, 'cache'),
          XDG_CONFIG_HOME: join(pnpmCache, 'config'),
          XDG_DATA_HOME: join(pnpmCache, 'data'),
          XDG_STATE_HOME: join(pnpmCache, 'state'),
          npm_config_userconfig: userConfig,
        },
      },
    )
    // pnpm's management metadata records the build store's absolute path. The
    // shipped read-only module tree needs only its portable packages and links.
    await cleanManagedNodeMetadata(nodeStage)
    await writeFile(join(staging, '.ready'), fingerprint)
    await rename(staging, ready)
  } finally {
    await rm(staging, { recursive: true, force: true })
  }
  return { python, node, fingerprint }
}
