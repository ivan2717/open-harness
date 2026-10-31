import { spawnSync } from 'node:child_process';
import { access, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(import.meta.url));
if (process.platform !== 'win32') throw new Error('The Windows Computer Use helper must be built on Windows.');
let output = path.join(root, 'bin');
let fixture = false;
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index++) {
  if (args[index] === '--fixture' && !fixture) fixture = true;
  else if (args[index] === '--output' && args[index + 1] && path.isAbsolute(args[index + 1])) output = args[++index];
  else throw new Error('Use --fixture and/or --output with an absolute directory.');
}
const framework = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'Microsoft.NET', 'Framework64', 'v4.0.30319');
const compiler = path.join(framework, 'csc.exe');
await access(compiler);
await mkdir(output, { recursive: true });
const reference = name => `/reference:${path.join(framework, name)}`;
const compile = (name, sources, references) => {
  const result = spawnSync(compiler, [
    // P/Invoke fills these interop fields, which C# cannot see statically.
    '/nologo', '/utf8output', '/target:winexe', '/platform:x64', '/optimize+', '/warnaserror+', '/nowarn:0649',
    `/win32manifest:${path.join(root, 'helper.manifest')}`, `/out:${path.join(output, name)}`,
    ...references.map(reference), ...sources.map(source => path.join(root, source)),
  ], { stdio: 'inherit', windowsHide: true, timeout: 120_000 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`The Windows Computer Use compiler failed (${result.status}).`);
};
compile('OpenHarnessComputerUse.exe', ['Protocol.cs', 'Native.cs', 'Identity.cs', 'Foreground.cs', 'Observation.cs', 'VisualGuard.cs', 'Input.cs', 'Program.cs'], [
  'System.dll', 'System.Core.dll', 'System.Drawing.dll', 'System.Web.Extensions.dll',
  'WPF/UIAutomationClient.dll', 'WPF/UIAutomationTypes.dll', 'WPF/WindowsBase.dll',
]);
const verification = spawnSync(path.join(output, 'OpenHarnessComputerUse.exe'), ['--self-test'], {
  stdio: 'inherit', windowsHide: true, timeout: 15_000,
});
if (verification.error) throw verification.error;
if (verification.status !== 0) throw new Error('The Windows Computer Use helper self-test failed.');
if (fixture) compile('ComputerUseFixture.exe', ['Fixture.cs'], [
  'System.dll', 'System.Drawing.dll', 'System.Windows.Forms.dll', 'System.Web.Extensions.dll',
]);
