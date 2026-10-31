import { createRequire } from 'node:module'
import { withLocalSigning } from './local-mac-signing.mjs'

const require = createRequire(import.meta.url)
// Resolve the signer used by our pinned electron-builder, including with pnpm.
const builderRequire = createRequire(require.resolve('app-builder-lib'))
const { signAsync } = builderRequire('@electron/osx-sign')

export default async function signLocalMac(options) {
  await withLocalSigning(async ({ keychain, fingerprint }) => {
    await signAsync({
      ...options,
      identity: fingerprint,
      keychain,
      // Self-signed local identities are intentionally not Apple-trusted. The
      // resulting signatures still validate and pin the actual certificate.
      identityValidation: false,
      preAutoEntitlements: false,
      preEmbedProvisioningProfile: false,
      optionsForFile: (file) => ({ ...options.optionsForFile?.(file), timestamp: 'none' }),
    })
  })
}
