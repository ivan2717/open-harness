import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
test('Windows live preview and virtual cursor lifecycle on a real test window', { skip: process.platform !== 'win32', timeout: 120_000 }, t => {
  const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-feedback-'));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const binary = path.join(temporary, 'feedback-test.exe');
  const framework = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'Microsoft.NET/Framework64/v4.0.30319');
  const options = { encoding: 'utf8', timeout: 90_000, windowsHide: true };
  execFileSync(path.join(framework, 'csc.exe'), ['/nologo', '/target:exe', '/platform:x64', '/warnaserror+', '/nowarn:0649',
    '/main:OpenHarness.ComputerUse.FeedbackTests', `/out:${binary}`, `/win32manifest:${path.join(root, 'windows/helper.manifest')}`,
    ...['System.dll', 'System.Core.dll', 'System.Drawing.dll', 'System.Windows.Forms.dll', 'System.Web.Extensions.dll',
      'WPF/UIAutomationClient.dll', 'WPF/UIAutomationTypes.dll', 'WPF/WindowsBase.dll'].map(name => `/reference:${path.join(framework, name)}`),
    ...readdirSync(path.join(root, 'windows')).filter(name => name.endsWith('.cs') && name !== 'Fixture.cs').map(name => path.join(root, 'windows', name)),
    path.join(root, 'tests/FeedbackTests.cs')], options);
  assert.match(execFileSync(binary, [], options), /Passed Windows feedback GUI acceptance/);
});
