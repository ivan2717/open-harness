import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { computerEndpoint, requireComputerEndpoint, requireComputerPlatform } from '../endpoint.mjs';

test('native endpoint follows the host OS and preserves explicit host configuration', () => {
  assert.ok(computerEndpoint({}, 'win32').endsWith(path.join('windows', 'bin', 'OpenHarnessComputerUse.exe')));
  assert.ok(computerEndpoint({}, 'darwin').endsWith(path.join('bin', 'Open Harness Computer Use.app')));
  const configured = path.resolve('selected-helper');
  assert.equal(computerEndpoint({ OPENHARNESS_COMPUTER_HELPER: configured }, 'win32'), configured);
  assert.equal(computerEndpoint({ OPENHARNESS_COMPUTER_APP: configured, OPENHARNESS_COMPUTER_HELPER: 'ignored' }, 'darwin'), configured);
  requireComputerPlatform('win32');
  requireComputerPlatform('darwin');
  assert.throws(() => requireComputerPlatform('linux'), /interactive desktop/);
});

test('native component validation rejects directories, relative paths and absent binaries', t => {
  const directory = mkdtempSync(path.join(tmpdir(), 'openharness-endpoint-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  assert.throws(() => requireComputerEndpoint('relative.exe'), /absolute path/);
  assert.throws(() => requireComputerEndpoint(directory), /missing or not executable/);
  assert.throws(() => requireComputerEndpoint(path.join(directory, 'missing.exe')), /missing or not executable/);
  const helper = path.join(directory, 'OpenHarnessComputerUse.exe');
  writeFileSync(helper, 'fixture only', { mode: 0o700 });
  requireComputerEndpoint(helper);
});
