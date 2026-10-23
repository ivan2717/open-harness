import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, cp, readFile, writeFile, rm, symlink } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import os from 'node:os';
import path from 'node:path';
import { generateKeyPairSync } from 'node:crypto';
import { EXTENSION_FILES, extensionIdFromKey, packageExtension, validateDistribution } from '../scripts/package-extension.mjs';
import { EXTENSION_ID } from '../constants.mjs';
const exec = promisify(execFile);
const sourceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
async function fixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-store-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await cp(path.join(sourceRoot, 'extension'), path.join(root, 'extension'), { recursive: true });
  await writeFile(path.join(root, 'distribution.json'), JSON.stringify({ chromeWebStoreUrl: null }));
  return root;
}
async function changeManifest(root, change) {
  const filename = path.join(root, 'extension/manifest.json'); const manifest = JSON.parse(await readFile(filename, 'utf8'));
  change(manifest); await writeFile(filename, JSON.stringify(manifest));
}
test('store archive contains only the extension runtime and icons, with manifest at ZIP root', async t => {
  const root = await fixture(t);
  await writeFile(path.join(root, 'extension/development-test.mjs'), 'secret test fixture');
  await writeFile(path.join(root, 'extension/private-key.pem'), 'DO NOT SHIP');
  await writeFile(path.join(root, 'native-host.mjs'), 'DO NOT SHIP');
  await writeFile(path.join(root, 'config.json'), JSON.stringify({ token: 'DO NOT SHIP' }));
  const output = await packageExtension({ root });
  const { stdout: entries } = await exec('unzip', ['-Z', '-1', output.zip]);
  assert.deepEqual(entries.trim().split('\n').sort(), [...EXTENSION_FILES].sort());
  const { stdout: manifestText } = await exec('unzip', ['-p', output.zip, 'manifest.json']);
  const manifest = JSON.parse(manifestText);
  assert.equal(manifest.key, undefined); assert.equal(manifest.icons['128'], 'icons/icon-128.png');
  assert.equal(output.chromeWebStoreUrl, null); assert.equal(output.storeConfigured, false);
  assert.equal(output.manifestKeyIncluded, false);
  await exec('unzip', ['-t', output.zip]);
  assert.ok(output.bytes > 1000);
});
test('repeated packaging is byte-for-byte reproducible', async t => {
  const root = await fixture(t); const first = await packageExtension({ root }); const second = await packageExtension({ root });
  assert.equal(first.sha256, second.sha256);
});
test('development identity is derived from the public key and mismatches block a release', async t => {
  const root = await fixture(t); const source = JSON.parse(await readFile(path.join(root, 'extension/manifest.json'), 'utf8'));
  assert.equal(extensionIdFromKey(source.key), EXTENSION_ID);
  const key = generateKeyPairSync('rsa', { modulusLength: 2048 }).publicKey.export({ type: 'spki', format: 'der' }).toString('base64');
  await changeManifest(root, manifest => { manifest.key = key; });
  await assert.rejects(packageExtension({ root }), /does not match EXTENSION_ID/);
});
test('release URLs must point to the exact store item; absent listing remains null', () => {
  assert.equal(validateDistribution({ chromeWebStoreUrl: null }), null);
  const url = `https://chromewebstore.google.com/detail/open-harness-chrome/${EXTENSION_ID}`;
  assert.equal(validateDistribution({ chromeWebStoreUrl: url }), url);
  for (const value of [url.replace('https:', 'http:'), `${url}?redirect=https://evil.example`, `${url}#install`, url.replace(EXTENSION_ID, 'a'.repeat(32)), url.replace('chromewebstore.google.com', 'chromewebstore.google.com.evil.example'), `https://user@chromewebstore.google.com/detail/${EXTENSION_ID}`]) assert.throws(() => validateDistribution({ chromeWebStoreUrl: value }));
  assert.throws(() => validateDistribution({ chromeWebStoreUrl: null, extensionId: 'override' }));
});
test('symlinked files and undeclared runtime dependencies cannot enter the archive', async t => {
  const root = await fixture(t);
  const filename = path.join(root, 'extension/background.mjs');
  await rm(filename); await symlink(path.join(sourceRoot, 'extension/background.mjs'), filename);
  await assert.rejects(packageExtension({ root }), /Refusing unexpected extension file/);
  await rm(filename); await writeFile(filename, "import { secret } from '../native-host.mjs';\n");
  await assert.rejects(packageExtension({ root }), /Unpackaged or external dependency/);
});
test('unexpected permissions and missing store icon block packaging', async t => {
  const root = await fixture(t); await changeManifest(root, manifest => { manifest.permissions.push('history'); });
  await assert.rejects(packageExtension({ root }), /Permission changes/);
  await changeManifest(root, manifest => { manifest.permissions = manifest.permissions.filter(permission => permission !== 'history'); delete manifest.icons['128']; });
  await assert.rejects(packageExtension({ root }), /Store icons are missing/);
});
