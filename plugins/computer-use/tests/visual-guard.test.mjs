import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
test('native visual guard handles local changes, noise, scaling and edges', { skip: !['darwin', 'win32'].includes(process.platform), timeout: 120_000 }, t => {
  const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-visual-guard-'));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const binary = path.join(temporary, process.platform === 'win32' ? 'visual-test.exe' : 'visual-test');
  const options = { encoding: 'utf8', timeout: 90_000, windowsHide: true };
  if (process.platform === 'darwin') {
    execFileSync('xcrun', ['swiftc', '-module-cache-path', path.join(temporary, 'modules'),
      path.join(root, 'native/VisualGuard.swift'), path.join(root, 'tests/VisualGuardTests.swift'), '-o', binary], options);
  } else {
    const framework = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'Microsoft.NET/Framework64/v4.0.30319');
    execFileSync(path.join(framework, 'csc.exe'), ['/nologo', '/target:exe', '/platform:x64', '/warnaserror+', `/out:${binary}`,
      ...['System.dll', 'System.Drawing.dll', 'System.Web.Extensions.dll'].map(name => `/reference:${path.join(framework, name)}`),
      path.join(root, 'windows/VisualGuard.cs'), path.join(root, 'tests/VisualGuardTests.cs')], options);
  }
  assert.match(execFileSync(binary, [path.join(root, 'tests/visual-guard-cases.json')], options), /Passed 13 visual guard cases/);
});
