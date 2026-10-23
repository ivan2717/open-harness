import { createHash, createPublicKey } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, mkdir, mkdtemp, readFile, rename, rm, utimes, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { EXTENSION_ID } from '../constants.mjs';
const exec = promisify(execFile);
const pluginRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const EXTENSION_FILES = Object.freeze([
  'manifest.json', 'background.mjs', 'controller.mjs', 'page.mjs', 'schema.mjs',
  'popup.html', 'popup.css', 'popup.mjs',
  'icons/icon-16.png', 'icons/icon-32.png', 'icons/icon-48.png', 'icons/icon-128.png',
]);
export function extensionIdFromKey(key) {
  if (typeof key !== 'string' || !/^[A-Za-z0-9+/]+={0,2}$/.test(key)) throw new Error('manifest.key must contain the base64 public key from the store dashboard.');
  const bytes = Buffer.from(key, 'base64');
  createPublicKey({ key: bytes, format: 'der', type: 'spki' });
  return [...createHash('sha256').update(bytes).digest().subarray(0, 16)].map(byte => String.fromCharCode(97 + (byte >> 4), 97 + (byte & 15))).join('');
}
export function validateDistribution(distribution, extensionId = EXTENSION_ID) {
  if (!distribution || typeof distribution !== 'object' || Array.isArray(distribution) || Object.keys(distribution).length !== 1 || !Object.hasOwn(distribution, 'chromeWebStoreUrl')) throw new Error('distribution.json must contain only chromeWebStoreUrl.');
  const value = distribution.chromeWebStoreUrl;
  if (value === null) return null;
  if (typeof value !== 'string') throw new Error('chromeWebStoreUrl must be null or a verified store URL.');
  const url = new URL(value);
  const match = /^\/detail\/(?:[a-z0-9][a-z0-9-]*\/)?([a-p]{32})\/?$/.exec(url.pathname);
  if (url.protocol !== 'https:' || url.hostname !== 'chromewebstore.google.com' || url.username || url.password || url.port || url.search || url.hash || !match || match[1] !== extensionId) throw new Error('Store URL must be the verified Chrome Web Store item matching EXTENSION_ID.');
  return value;
}
export async function packageExtension({ root = pluginRoot, outputPath } = {}) {
  const extensionRoot = path.join(root, 'extension');
  const rootStat = await lstat(extensionRoot);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) throw new Error('Extension source must be a regular directory.');
  const iconsStat = await lstat(path.join(extensionRoot, 'icons'));
  if (!iconsStat.isDirectory() || iconsStat.isSymbolicLink()) throw new Error('Extension icons must be a regular directory.');
  const manifestPath = path.join(extensionRoot, 'manifest.json');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (manifest.manifest_version !== 3 || !/^\d+(?:\.\d+){0,3}$/.test(manifest.version) || manifest.version.split('.').some(part => Number(part) > 65535 || (part.length > 1 && part.startsWith('0'))) || !manifest.version.split('.').some(part => Number(part) > 0)) throw new Error('A valid MV3 manifest and extension version are required.');
  if (extensionIdFromKey(manifest.key) !== EXTENSION_ID) throw new Error('Manifest public key does not match EXTENSION_ID. Synchronize the official store ID/public key and desktop host before packaging.');
  const storeUrl = validateDistribution(JSON.parse(await readFile(path.join(root, 'distribution.json'), 'utf8')));
  const expectedPermissions = ['debugger', 'tabs', 'storage', 'nativeMessaging'];
  if (!Array.isArray(manifest.permissions) || [...manifest.permissions].sort().join(',') !== expectedPermissions.sort().join(',')) throw new Error('Permission changes require updating the store disclosures and package allowlist.');
  if (manifest.background?.service_worker !== 'background.mjs' || manifest.action?.default_popup !== 'popup.html' || manifest.externally_connectable || manifest.host_permissions || manifest.web_accessible_resources) throw new Error('Unexpected extension entry points or host access. Review before publishing.');
  for (const size of [16, 32, 48, 128]) if (manifest.icons?.[size] !== `icons/icon-${size}.png`) throw new Error('Store icons are missing from the manifest.');
  const destination = path.resolve(outputPath ?? path.join(root, 'dist', `open-harness-chrome-${manifest.version}.zip`));
  if (!destination.endsWith('.zip') || destination === extensionRoot || destination.startsWith(`${extensionRoot}${path.sep}`)) throw new Error('Choose a .zip output outside the extension source directory.');
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-package-'));
  try {
    for (const filename of EXTENSION_FILES) {
      const source = path.join(extensionRoot, filename); const stat = await lstat(source);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 2 * 1024 * 1024) throw new Error(`Refusing unexpected extension file: ${filename}`);
      let bytes = await readFile(source);
      if (filename === 'manifest.json') {
        // The store signs its package. The source key stabilizes unpacked
        // development IDs; it is not a request for a particular store item ID.
        const { key, ...storeManifest } = manifest;
        bytes = Buffer.from(`${JSON.stringify(storeManifest, null, 2)}\n`);
      } else if (filename.endsWith('.mjs')) {
        const sourceText = bytes.toString('utf8');
        for (const match of sourceText.matchAll(/\b(?:import|export)\s[^;]*?\bfrom\s*['"]([^'"]+)['"]/g)) {
          const dependency = path.posix.normalize(path.posix.join(path.posix.dirname(filename), match[1]));
          if (!match[1].startsWith('./') || !EXTENSION_FILES.includes(dependency)) throw new Error(`Unpackaged or external dependency in ${filename}.`);
        }
      } else if (filename.endsWith('.png')) {
        const expected = Number(/icon-(\d+)\.png$/.exec(filename)[1]);
        if (!bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) || bytes.readUInt32BE(16) !== expected || bytes.readUInt32BE(20) !== expected) throw new Error(`Invalid PNG icon dimensions: ${filename}`);
      }
      const staged = path.join(temporary, filename); await mkdir(path.dirname(staged), { recursive: true }); await writeFile(staged, bytes, { mode: 0o644 });
      // Stable ZIP metadata keeps repeated local packaging byte-for-byte equal.
      await utimes(staged, new Date('2020-01-01T00:00:00Z'), new Date('2020-01-01T00:00:00Z'));
    }
    const archive = path.join(temporary, 'extension.zip');
    await exec(process.platform === 'darwin' ? '/usr/bin/zip' : 'zip', ['-X', '-9', '-q', archive, ...EXTENSION_FILES], { cwd: temporary, env: { ...process.env, TZ: 'UTC' } });
    const bytes = await readFile(archive); await mkdir(path.dirname(destination), { recursive: true });
    // Copy to the destination filesystem before atomic rename (tmp may differ).
    const stagedOutput = `${destination}.${process.pid}.tmp`; await writeFile(stagedOutput, bytes, { mode: 0o644 });
    try { await rename(stagedOutput, destination); } finally { await rm(stagedOutput, { force: true }); }
    return { zip: destination, version: manifest.version, extensionId: EXTENSION_ID, chromeWebStoreUrl: storeUrl, storeConfigured: storeUrl !== null, sha256: createHash('sha256').update(bytes).digest('hex'), bytes: bytes.length, files: EXTENSION_FILES, manifestKeyIncluded: false };
  } finally { await rm(temporary, { recursive: true, force: true }); }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length && (args.length !== 2 || args[0] !== '--out')) { process.stderr.write('Usage: node scripts/package-extension.mjs [--out /absolute/path/extension.zip]\n'); process.exitCode = 1; }
  else {
    try { process.stdout.write(`${JSON.stringify(await packageExtension({ outputPath: args[1] }), null, 2)}\n`); }
    catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
  }
}
