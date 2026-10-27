import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { access, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, resolve, win32 } from 'node:path'

export interface WorkspaceInterpreter {
  executable: string
  args: string[]
  environment: Record<string, string>
  version: string
  source: 'bundled' | 'system'
  libraries: { name: string; version?: string }[]
  read_paths: string[]
  module_paths?: string[]
  package_manager?: {
    name: 'pnpm'
    executable: string
    args: string[]
    environment: Record<string, string>
  }
}

export interface WorkspaceRuntimeManifest {
  version: 1
  node: WorkspaceInterpreter | null
  python: WorkspaceInterpreter | null
  notes: string[]
}

interface RuntimeOptions {
  directory: string
  platform: NodeJS.Platform
  resourcesDirectory: string
  environment?: NodeJS.ProcessEnv
  probe?: (executable: string, args: string[]) => Promise<string>
}

// -I excludes the current directory, PYTHONPATH and user site packages. Merely
// inspecting distributions does not import project code or third-party modules.
const pythonProbe = `import importlib.metadata,json,os,sys
names=[('numpy','numpy'),('pandas','pandas'),('Pillow','PIL'),('python-docx','docx'),('python-pptx','pptx'),('openpyxl','openpyxl'),('XlsxWriter','xlsxwriter'),('pypdf','pypdf'),('matplotlib','matplotlib'),('reportlab','reportlab'),('pip','pip')]
libraries=[]
for distribution,module in names:
 try: libraries.append({'name':module,'version':importlib.metadata.version(distribution)})
 except importlib.metadata.PackageNotFoundError: pass
print(json.dumps({'executable':sys.executable,'version':'.'.join(map(str,sys.version_info[:3])),'libraries':libraries,'read_paths':list(dict.fromkeys([os.path.dirname(sys.executable),sys.prefix,sys.base_prefix]+[p for p in sys.path if p and os.path.isabs(p)]))}))`

function probeProcess(executable: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      executable,
      args,
      {
        timeout: 3_000,
        maxBuffer: 32_768,
        windowsHide: true,
        encoding: 'utf8',
        env: { ...process.env, NODE_OPTIONS: '', NODE_PATH: '', ELECTRON_RUN_AS_NODE: '' },
      },
      (error, stdout) => (error ? reject(error) : resolve(stdout)),
    )
  })
}

function absolute(path: string, platform: NodeJS.Platform): boolean {
  return platform === 'win32'
    ? win32.isAbsolute(path) && /^(?:[a-z]:[\\/]|\\\\[^\\]+\\[^\\]+)/i.test(path)
    : isAbsolute(path)
}

/** Resolve executables without a shell and avoid Windows Store launcher aliases. */
export async function pythonCandidates(
  options: Pick<RuntimeOptions, 'platform' | 'environment' | 'resourcesDirectory'>,
): Promise<{ executable: string; args: string[]; source: 'bundled' | 'system' }[]> {
  const environment = options.environment ?? process.env
  const path = options.platform === 'win32' ? win32 : { join, resolve, dirname }
  const candidates: { executable: string; args: string[]; source: 'bundled' | 'system' }[] = []
  const add = (
    executable: string,
    args: string[] = [],
    source: 'bundled' | 'system' = 'system',
  ) => {
    if (
      absolute(executable, options.platform) &&
      !/[\\/]Microsoft[\\/]WindowsApps[\\/]/i.test(executable) &&
      !candidates.some((candidate) => candidate.executable === executable)
    )
      candidates.push({ executable, args, source })
  }
  // Builds ship a standalone interpreter. Discovery never downloads code,
  // installs packages, or silently adopts a user's system Python.
  add(
    path.join(
      options.resourcesDirectory,
      'workspace-runtime',
      'python',
      ...(options.platform === 'win32' ? ['python.exe'] : ['bin', 'python3']),
    ),
    [],
    'bundled',
  )
  if (environment.OPENHARNESS_PYTHON_BINARY) add(environment.OPENHARNESS_PYTHON_BINARY)
  if (environment.OPENHARNESS_ALLOW_SYSTEM_PYTHON !== '1') {
    const present = await Promise.all(
      candidates.map(async (candidate) => ({
        candidate,
        exists: await access(candidate.executable).then(
          () => true,
          () => false,
        ),
      })),
    )
    return present.filter(({ exists }) => exists).map(({ candidate }) => candidate)
  }
  if (options.platform === 'darwin') {
    add('/opt/homebrew/bin/python3')
    add('/usr/local/bin/python3')
  }
  const pathVariable = environment.PATH ?? environment.Path ?? ''
  for (const directory of pathVariable.split(options.platform === 'win32' ? ';' : ':')) {
    if (!absolute(directory, options.platform)) continue
    for (const name of options.platform === 'win32' ? ['python.exe', 'python3.exe'] : ['python3'])
      add(path.join(directory, name))
  }
  if (options.platform === 'win32') {
    if (environment.LOCALAPPDATA)
      add(path.join(environment.LOCALAPPDATA, 'Programs', 'Python', 'Launcher', 'py.exe'), ['-3'])
    if (environment.SystemRoot) add(path.join(environment.SystemRoot, 'py.exe'), ['-3'])
    for (const directory of pathVariable.split(';'))
      if (absolute(directory, options.platform)) add(path.join(directory, 'py.exe'), ['-3'])
  } else add('/usr/bin/python3')
  const present = await Promise.all(
    candidates.map(async (candidate) => ({
      candidate,
      exists: await access(candidate.executable).then(
        () => true,
        () => false,
      ),
    })),
  )
  return present
    .filter(({ exists }) => exists)
    .map(({ candidate }) => candidate)
    .slice(0, 4)
}

export function parsePythonProbe(
  value: string,
  platform: NodeJS.Platform,
  source: 'bundled' | 'system',
): WorkspaceInterpreter {
  const candidate: unknown = JSON.parse(value)
  if (!candidate || typeof candidate !== 'object') throw new Error('Invalid Python runtime')
  const result = candidate as Record<string, unknown>
  if (
    typeof result.executable !== 'string' ||
    !absolute(result.executable, platform) ||
    typeof result.version !== 'string' ||
    !/^3\.\d+\.\d+$/.test(result.version) ||
    !Array.isArray(result.libraries) ||
    result.libraries.length > 32 ||
    !Array.isArray(result.read_paths) ||
    result.read_paths.length > 64 ||
    !result.read_paths.every((path) => typeof path === 'string' && absolute(path, platform))
  )
    throw new Error('Invalid Python runtime')
  const libraries = result.libraries.map((library: unknown) => {
    if (!library || typeof library !== 'object') throw new Error('Invalid Python library')
    const item = library as Record<string, unknown>
    if (
      typeof item.name !== 'string' ||
      !/^[A-Za-z0-9_-]{1,64}$/.test(item.name) ||
      typeof item.version !== 'string' ||
      item.version.length > 64
    )
      throw new Error('Invalid Python library')
    return { name: item.name, version: item.version }
  })
  return {
    executable: result.executable,
    args: ['-I', '-B'],
    environment: {},
    version: result.version,
    source,
    libraries,
    read_paths: result.read_paths as string[],
  }
}

async function discoverNode(options: RuntimeOptions): Promise<WorkspaceInterpreter | null> {
  const paths = options.platform === 'win32' ? win32 : { dirname, resolve, join }
  const nodeRoot = paths.join(options.resourcesDirectory, 'workspace-runtime', 'node')
  const executable = paths.join(
    nodeRoot,
    ...(options.platform === 'win32' ? ['node.exe'] : ['bin', 'node']),
  )
  let metadata: { version: string; platform: string; architecture: string }
  try {
    await access(executable)
    metadata = JSON.parse(
      await (options.probe ?? probeProcess)(executable, [
        '-e',
        'console.log(JSON.stringify({version:process.versions.node,platform:process.platform,architecture:process.arch}))',
      ]),
    )
    if (
      !/^\d+\.\d+\.\d+$/.test(metadata.version) ||
      Number(metadata.version.split('.')[0]) < 22 ||
      metadata.platform !== options.platform ||
      metadata.architecture !== process.arch
    )
      return null
  } catch {
    return null
  }
  const node: WorkspaceInterpreter = {
    executable,
    args: [],
    environment: {},
    version: metadata.version,
    source: 'bundled',
    libraries: ['fs', 'path', 'os', 'crypto', 'stream', 'zlib', 'util'].map((name) => ({
      name: `node:${name}`,
    })),
    read_paths: [nodeRoot],
    module_paths: [],
  }
  const nodeDependencies = paths.join(
    options.resourcesDirectory,
    'workspace-runtime',
    'node',
    'node_modules',
  )
  const managedLibraries: { name: string; version?: string }[] = []
  for (const name of ['csv-parse', 'csv-stringify', 'pngjs', 'pnpm']) {
    try {
      const metadata = JSON.parse(
        await readFile(paths.join(nodeDependencies, name, 'package.json'), 'utf8'),
      )
      if (metadata.name === name && typeof metadata.version === 'string')
        managedLibraries.push({ name, version: metadata.version })
    } catch {
      /* Missing development resources are reported as unavailable. */
    }
  }
  if (managedLibraries.length) {
    node.libraries.push(...managedLibraries)
    node.read_paths.push(paths.dirname(nodeDependencies))
    node.module_paths = [nodeDependencies]
  }
  const pnpmCli = paths.join(nodeDependencies, 'pnpm', 'bin', 'pnpm.cjs')
  if (
    managedLibraries.some(({ name }) => name === 'pnpm') &&
    (await access(pnpmCli).then(
      () => true,
      () => false,
    ))
  ) {
    node.package_manager = {
      name: 'pnpm',
      executable,
      args: [pnpmCli],
      environment: {},
    }
  }
  return node
}

export async function discoverWorkspaceRuntime(
  options: RuntimeOptions,
): Promise<WorkspaceRuntimeManifest> {
  const node = await discoverNode(options)
  const nodeNotes = node
    ? []
    : [
        'The managed standalone Node runtime is missing or unavailable. Rebuild/reinstall Open Harness; no Electron or system Node fallback was activated.',
      ]
  for (const candidate of await pythonCandidates(options)) {
    try {
      const output = await (options.probe ?? probeProcess)(candidate.executable, [
        ...candidate.args,
        '-I',
        '-B',
        '-c',
        pythonProbe,
      ])
      const python = parsePythonProbe(output, options.platform, candidate.source)
      await access(python.executable)
      return {
        version: 1,
        node,
        python,
        notes: [
          ...nodeNotes,
          'Use Python for document, spreadsheet, image and data processing; use Node for JavaScript, JSON and file orchestration. Choose by task, not a fixed language priority.',
          'All conversations use the same managed runtimes and preinstalled libraries. Discovery does not create virtual environments or change package-manager directories. Never modify these shared application resources or install packages globally.',
          'Prefer the shared libraries. When additional dependencies are necessary, explicitly use an appropriate project or temporary environment through an approved command; follow the project toolchain and lockfile for project dependency changes. Runtime discovery does not grant network or filesystem access.',
          ...(python.source === 'system'
            ? [
                'An explicitly enabled system Python fallback is in use; its packages may differ from the managed runtime.',
              ]
            : []),
        ],
      }
    } catch {
      // A broken interpreter must not stop desktop startup. Report the
      // remaining validated runtime and try the next bounded Python candidate.
    }
  }
  return {
    version: 1,
    node,
    python: null,
    notes: [
      ...nodeNotes,
      'The managed Python runtime is missing or unavailable. Rebuild/reinstall Open Harness; system Python discovery requires an explicit OPENHARNESS_PYTHON_BINARY or OPENHARNESS_ALLOW_SYSTEM_PYTHON=1. No interpreter or packages were downloaded.',
    ],
  }
}

export async function prepareWorkspaceRuntime(options: RuntimeOptions): Promise<NodeJS.ProcessEnv> {
  const manifest = await discoverWorkspaceRuntime(options)
  await mkdir(options.directory, { recursive: true, mode: 0o700 })
  const destination = join(options.directory, 'runtime.json')
  const contents = `${JSON.stringify(manifest, null, 2)}\n`
  // Avoid rewriting an unchanged manifest; atomic replacement means the
  // backend can never observe a partial update after an interrupted startup.
  if ((await readFile(destination, 'utf8').catch(() => '')) !== contents) {
    const temporary = join(options.directory, `.${randomUUID()}.tmp`)
    try {
      await writeFile(temporary, contents, { mode: 0o600, flag: 'wx' })
      await rename(temporary, destination)
    } finally {
      await rm(temporary, { force: true })
    }
  }
  return { OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST: destination }
}
