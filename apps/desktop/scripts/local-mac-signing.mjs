import { spawnSync } from 'node:child_process'
import { createHash, randomBytes, X509Certificate } from 'node:crypto'
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'

// A private, persistent development identity. Never add a trusted root, change
// system trust policy, or fall back to an identifier-only/ad-hoc requirement.
export const localSigningDirectory = () =>
  join(homedir(), 'Library', 'Application Support', 'Open Harness Development', 'signing')

function run(program, args, secrets = []) {
  const result = spawnSync(program, args, { encoding: 'utf8', timeout: 120_000 })
  if (result.error || result.status !== 0) {
    let message = result.error?.message ?? result.stderr ?? 'command failed'
    for (const secret of secrets) message = message.replaceAll(secret, '<redacted>')
    throw new Error(`Local signing: ${program}: ${message}`)
  }
  return result.stdout
}

async function privateEntry(path, directory = false) {
  const info = await lstat(path)
  if (
    info.isSymbolicLink() ||
    info.uid !== process.getuid() ||
    (directory ? !info.isDirectory() : !info.isFile()) ||
    (info.mode & 0o077) !== 0
  )
    throw new Error(
      `Local signing requires a private, user-owned ${directory ? 'directory' : 'file'}: ${path}`,
    )
}

const keychains = () =>
  run('/usr/bin/security', ['list-keychains', '-d', 'user'])
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => JSON.parse(line))

async function createIdentity(root) {
  const stage = await mkdtemp(join(root, '.create-'))
  const keychain = join(stage, 'identity.keychain-db')
  const password = randomBytes(32).toString('hex')
  try {
    await writeFile(
      join(stage, 'openssl.cnf'),
      '[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n' +
        '[dn]\nCN=Open Harness Local Development\n' +
        '[ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n' +
        'extendedKeyUsage=codeSigning\n',
      { mode: 0o600 },
    )
    run('/usr/bin/openssl', [
      'req',
      '-x509',
      '-newkey',
      'rsa:3072',
      '-nodes',
      '-days',
      '3650',
      '-config',
      join(stage, 'openssl.cnf'),
      '-keyout',
      join(stage, 'key.pem'),
      '-out',
      join(stage, 'certificate.pem'),
    ])
    await chmod(join(stage, 'certificate.pem'), 0o600)
    run(
      '/usr/bin/openssl',
      [
        'pkcs12',
        '-export',
        '-inkey',
        join(stage, 'key.pem'),
        '-in',
        join(stage, 'certificate.pem'),
        '-out',
        join(stage, 'identity.p12'),
        '-passout',
        `pass:${password}`,
      ],
      [password],
    )
    run('/usr/bin/security', ['create-keychain', '-p', password, keychain], [password])
    run('/usr/bin/security', ['unlock-keychain', '-p', password, keychain], [password])
    run(
      '/usr/bin/security',
      [
        'import',
        join(stage, 'identity.p12'),
        '-k',
        keychain,
        '-P',
        password,
        '-T',
        '/usr/bin/codesign',
      ],
      [password],
    )
    run('/usr/bin/security', ['lock-keychain', keychain])
    await chmod(keychain, 0o600)
    await writeFile(join(stage, 'password'), password, { mode: 0o600 })
    // Publish only complete identities. A damaged existing identity is an error,
    // never a reason to silently generate a new certificate and lose TCC grants.
    for (const filename of ['key.pem', 'identity.p12', 'openssl.cnf'])
      await rm(join(stage, filename))
    await rename(stage, join(root, 'identity'))
  } finally {
    await rm(stage, { recursive: true, force: true })
  }
}

export async function withLocalSigning(action, root = localSigningDirectory()) {
  if (process.platform !== 'darwin') throw new Error('Local macOS signing requires macOS')
  await mkdir(root, { recursive: true, mode: 0o700 })
  await privateEntry(root, true)
  root = await realpath(root)
  const lock = join(root, 'build.lock')
  await writeFile(lock, `${process.pid}\n`, { flag: 'wx', mode: 0o600 }).catch((error) => {
    if (error.code === 'EEXIST')
      throw new Error(
        `Another local signing operation is active. If its process exited, remove ${lock}.`,
      )
    throw error
  })
  const identityRoot = join(root, 'identity')
  const keychain = join(identityRoot, 'identity.keychain-db')
  let registered = false
  try {
    const exists = await lstat(identityRoot).then(
      () => true,
      (error) => {
        if (error.code === 'ENOENT') return false
        throw error
      },
    )
    if (!exists) await createIdentity(root)
    await privateEntry(identityRoot, true)
    for (const name of ['certificate.pem', 'password', 'identity.keychain-db'])
      await privateEntry(join(identityRoot, name))
    const certificate = new X509Certificate(await readFile(join(identityRoot, 'certificate.pem')))
    if (new Date(certificate.validTo).getTime() <= Date.now())
      throw new Error(
        'Local signing certificate expired. Renew it explicitly; system permissions will need refreshing.',
      )
    const fingerprint = createHash('sha1').update(certificate.raw).digest('hex').toUpperCase()
    const password = await readFile(join(identityRoot, 'password'), 'utf8')
    run('/usr/bin/security', ['unlock-keychain', '-p', password, keychain], [password])
    run('/usr/bin/security', ['set-keychain-settings', '-lut', '900', keychain])
    const original = keychains()
    if (!original.some((entry) => resolve(entry) === resolve(keychain))) {
      // codesign needs the certificate in the search list even with --keychain.
      // This is a search path, not certificate trust, and is undone after signing.
      run('/usr/bin/security', ['list-keychains', '-d', 'user', '-s', ...original, keychain])
    }
    // Also recover a search-list entry left by an interrupted previous build.
    registered = true
    return await action({ keychain, fingerprint })
  } finally {
    try {
      if (registered)
        run('/usr/bin/security', [
          'list-keychains',
          '-d',
          'user',
          '-s',
          ...keychains().filter((entry) => resolve(entry) !== resolve(keychain)),
        ])
    } finally {
      // Lock even if signing or search-list cleanup failed. Do not expose private
      // keys/certificate passwords in logs, the repository or build artifacts.
      spawnSync('/usr/bin/security', ['lock-keychain', keychain], { stdio: 'ignore' })
      await rm(lock, { force: true })
    }
  }
}
