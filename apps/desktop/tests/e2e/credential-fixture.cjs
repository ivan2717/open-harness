// Test-process preload, never bundled with the application. Regression tests use
// synthetic credentials and a temporary userData directory. Do not access the OS
// keychain: this suite validates persistence/IPC, not operating-system consent.
if (process.versions.electron && process.type === 'browser') {
  globalThis.__credentialFixtureActive = true
  const { app, safeStorage } = require('electron')
  const { createCipheriv, createDecipheriv, randomBytes } = require('node:crypto')
  const { isAbsolute, relative, sep } = require('node:path')
  const root = process.env.OPENHARNESS_E2E_TEMP_ROOT
  const directory = process.env.OPENHARNESS_DESKTOP_DATA_DIR
  const key = process.env.OPENHARNESS_E2E_CIPHER_KEY
  const inside = root && directory && relative(root, directory)
  if (
    app.isPackaged ||
    !root ||
    !isAbsolute(root) ||
    !inside ||
    inside.startsWith(`..${sep}`) ||
    inside === '..' ||
    isAbsolute(inside) ||
    !/^[a-f0-9]{64}$/.test(key ?? '')
  ) {
    throw new Error('Credential fixture requires an unpackaged app and isolated test data')
  }
  const encryptionKey = Buffer.from(key, 'hex')
  safeStorage.isAsyncEncryptionAvailable = async () => true
  safeStorage.getSelectedStorageBackend = () => 'gnome_libsecret'
  safeStorage.encryptStringAsync = async (text) => {
    const iv = randomBytes(12)
    const cipher = createCipheriv('aes-256-gcm', encryptionKey, iv)
    const encrypted = Buffer.concat([cipher.update(text, 'utf8'), cipher.final()])
    return Buffer.concat([iv, cipher.getAuthTag(), encrypted])
  }
  safeStorage.decryptStringAsync = async (bytes) => {
    if (process.env.OPENHARNESS_E2E_UNLOCK_GATE === '1') {
      await new Promise((resolve, reject) => {
        globalThis.__fixtureUnlock = (allowed) => {
          delete globalThis.__fixtureUnlock
          if (allowed) resolve()
          else reject(new Error('Synthetic OS unlock cancelled'))
        }
      })
    }
    const buffer = Buffer.from(bytes)
    const cipher = createDecipheriv('aes-256-gcm', encryptionKey, buffer.subarray(0, 12))
    cipher.setAuthTag(buffer.subarray(12, 28))
    return {
      result: Buffer.concat([cipher.update(buffer.subarray(28)), cipher.final()]).toString('utf8'),
      shouldReEncrypt: false,
    }
  }
  // Detect accidental regressions to the blocking API instead of touching the OS.
  for (const name of ['isEncryptionAvailable', 'encryptString', 'decryptString']) {
    safeStorage[name] = () => {
      throw new Error('Blocking credential API used by regression app')
    }
  }
}
