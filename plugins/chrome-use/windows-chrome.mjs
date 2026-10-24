import { execFile } from 'node:child_process';
import { lstat } from 'node:fs/promises';
import path from 'node:path';
import { promisify } from 'node:util';

const execute = promisify(execFile);
// reg.exe uses the console code page, which corrupts non-ASCII installation
// paths when interpreted as UTF-8. Return UTF-8 bytes encoded as ASCII instead.
// Hive/view are fixed environment values; no path is interpolated into script.
const readAppPath = String.raw`
$ErrorActionPreference = 'Stop'
$hive = [Microsoft.Win32.RegistryHive]::CurrentUser
if ($env:OPENHARNESS_CHROME_HIVE -eq 'HKLM') { $hive = [Microsoft.Win32.RegistryHive]::LocalMachine }
$view = [Microsoft.Win32.RegistryView]::Registry64
if ($env:OPENHARNESS_CHROME_VIEW -eq '32') { $view = [Microsoft.Win32.RegistryView]::Registry32 }
$base = $null; $key = $null
try {
  $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
  $key = $base.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe')
  if ($null -ne $key) {
    $value = $key.GetValue('')
    if ($value -is [string]) { [Console]::Out.Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value))) }
  }
} finally {
  if ($null -ne $key) { $key.Dispose() }
  if ($null -ne $base) { $base.Dispose() }
}`;

/** Locate installed Chrome without a shell, PATH lookup or profile mutation. */
export async function findChromeExecutable({ env = process.env, inspectFile = lstat, executeFile = execute } = {}) {
  const candidates = [];
  const systemRoot = env.SystemRoot ?? env.SYSTEMROOT;
  if (systemRoot && path.win32.isAbsolute(systemRoot)) {
    const powershell = path.win32.join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
    for (const hive of ['HKCU', 'HKLM']) {
      for (const view of ['64', '32']) {
        try {
          const { stdout } = await executeFile(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', readAppPath], {
            env: { ...env, OPENHARNESS_CHROME_HIVE: hive, OPENHARNESS_CHROME_VIEW: view },
            windowsHide: true, timeout: 3000, maxBuffer: 16384,
          });
          const encoded = stdout.trim();
          if (encoded && /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) candidates.push(Buffer.from(encoded, 'base64').toString('utf8').trim().replace(/^"([^"]+)"$/, '$1'));
        } catch { /* Missing registry view/key is normal; try the next installation location. */ }
      }
    }
  }
  for (const root of [env.LOCALAPPDATA, env.ProgramW6432, env.ProgramFiles, env['ProgramFiles(x86)']]) {
    if (root && path.win32.isAbsolute(root)) candidates.push(path.win32.join(root, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  }
  for (const candidate of new Set(candidates)) {
    // App Paths values are an executable path, never a command with arguments.
    if (!path.win32.isAbsolute(candidate) || candidate.startsWith('\\\\') || /["\0\r\n]/.test(candidate) || path.win32.basename(candidate).toLowerCase() !== 'chrome.exe') continue;
    try {
      const stat = await inspectFile(candidate);
      if (stat.isFile() && !stat.isSymbolicLink()) return candidate;
    } catch { /* Chrome may be installed for a different user or removed. */ }
  }
  throw new Error('Google Chrome was not found. Install Chrome, then prepare its connection in Open Harness.');
}
