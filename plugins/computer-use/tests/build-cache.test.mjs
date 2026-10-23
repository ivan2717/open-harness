import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { appendFileSync, cpSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const hash = file => createHash('sha256').update(readFileSync(file)).digest('hex');

test('unchanged builds retain binary bytes/mtime while a source change rebuilds', { skip: process.platform !== 'darwin', timeout: 120_000 }, t => {
  const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-helper-build-test-'));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  // Build a disposable package: this test never replaces the helper the user granted.
  for (const item of ['native', 'build.mjs', 'bin']) cpSync(path.join(root, item), path.join(temporary, item), { recursive: true, preserveTimestamps: true });
  const executable = path.join(temporary, 'bin/computer-helper');
  const stamp = path.join(temporary, 'bin/build-stamp.json');
  function build() {
    const result = spawnSync(process.execPath, [path.join(temporary, 'build.mjs')], { encoding: 'utf8', timeout: 90_000 });
    assert.equal(result.status, 0, result.stderr || result.error?.message);
    return result.stdout;
  }
  build(); // The source checkout may contain the previous helper revision.
  const before = { hash: hash(executable), mtime: statSync(executable).mtimeMs };
  const app = path.join(temporary, 'bin', 'Open Harness Computer Use.app');
  const appExecutable = path.join(app, 'Contents/MacOS/OpenHarnessComputerUse');
  const appBefore = { hash: hash(appExecutable), mtime: statSync(appExecutable).mtimeMs };
  const info = readFileSync(path.join(app, 'Contents/Info.plist'), 'utf8');
  assert.match(info, /<key>CFBundleIdentifier<\/key><string>dev\.openharness\.computeruse<\/string>/);
  assert.match(info, /<key>CFBundleDisplayName<\/key><string>Open Harness Computer Use<\/string>/);
  assert.match(info, /<key>LSUIElement<\/key><true\/>/);
  assert.match(build(), /unchanged/);
  assert.match(build(), /unchanged/);
  assert.equal(hash(executable), before.hash);
  assert.equal(statSync(executable).mtimeMs, before.mtime);
  assert.equal(hash(appExecutable), appBefore.hash);
  assert.equal(statSync(appExecutable).mtimeMs, appBefore.mtime);
  const previousInputs = JSON.parse(readFileSync(stamp, 'utf8')).inputs_hash;
  appendFileSync(path.join(temporary, 'native/Access.swift'), '\n// Build cache regression: source bytes changed.\n');
  assert.match(build(), /Built macOS/);
  assert.notEqual(JSON.parse(readFileSync(stamp, 'utf8')).inputs_hash, previousInputs);
  assert.notEqual(statSync(executable).mtimeMs, before.mtime);
  assert.match(build(), /unchanged/);
});
