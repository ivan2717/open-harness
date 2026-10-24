import { execFile } from 'node:child_process'
import { win32 } from 'node:path'
import { promisify } from 'node:util'

const execute = promisify(execFile)
const registryKey = 'HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\dev.openharness.chrome'
const windowsSystem = (name: string) =>
  win32.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', name)
const powershell = windowsSystem('WindowsPowerShell\\v1.0\\powershell.exe')
const encodedScript = (script: string) => [
  '-NoLogo',
  '-NoProfile',
  '-NonInteractive',
  '-EncodedCommand',
  Buffer.from(script, 'utf16le').toString('base64'),
]

/** Register only for this Windows account; no elevation or browser policies. */
export async function registerWindowsChromeHost(manifest: string): Promise<void> {
  for (const view of ['32', '64']) {
    await execute(
      windowsSystem('reg.exe'),
      ['ADD', registryKey, '/ve', '/t', 'REG_SZ', '/d', manifest, '/f', `/reg:${view}`],
      { timeout: 10_000, windowsHide: true },
    )
  }
}

export async function windowsChromeHostRegistered(manifest: string): Promise<boolean> {
  for (const view of ['32', '64']) {
    try {
      // reg.exe's output uses the system code page. An ASCII envelope preserves
      // Chinese/Unicode paths regardless of the user's Windows language.
      const { stdout } = await execute(
        powershell,
        encodedScript(`
$ErrorActionPreference = 'Stop'
$view = [Enum]::Parse([Microsoft.Win32.RegistryView], 'Registry' + $env:OPENHARNESS_CHROME_REGISTRY_VIEW)
$base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
try {
  $key = $base.OpenSubKey('Software\\Google\\Chrome\\NativeMessagingHosts\\dev.openharness.chrome')
  if ($null -ne $key) {
    try {
      $value = $key.GetValue('')
      if ($value -is [string]) { [Console]::Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value))) }
    } finally { $key.Dispose() }
  }
} finally { $base.Dispose() }
`),
        {
          env: { ...process.env, OPENHARNESS_CHROME_REGISTRY_VIEW: view },
          timeout: 5_000,
          windowsHide: true,
        },
      )
      const encoded = stdout.trim()
      const value = /^[A-Za-z0-9+/]+={0,2}$/.test(encoded)
        ? Buffer.from(encoded, 'base64').toString('utf8')
        : undefined
      if (
        !value ||
        win32.normalize(value).toLowerCase() !== win32.normalize(manifest).toLowerCase()
      )
        return false
    } catch {
      return false
    }
  }
  return true
}

/** Replace inherited access with current-user and SYSTEM access on the bridge directory. */
export async function secureWindowsChromeDirectory(directory: string): Promise<void> {
  // Fixed script and a separate environment value keep spaces, Unicode, %, &
  // and other legal Windows path characters out of command syntax.
  const script = `
$ErrorActionPreference = 'Stop'
$path = $env:OPENHARNESS_CHROME_PRIVATE_DIRECTORY
$item = Get-Item -LiteralPath $path -Force
if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Invalid Chrome directory' }
$user = [Security.Principal.WindowsIdentity]::GetCurrent().User
$system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($user)
$acl.SetAccessRuleProtection($true, $false)
foreach ($identity in @($user, $system)) {
  $rule = [Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
  $acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $path -AclObject $acl
`
  await execute(powershell, encodedScript(script), {
    env: { ...process.env, OPENHARNESS_CHROME_PRIVATE_DIRECTORY: directory },
    timeout: 15_000,
    windowsHide: true,
  })
}
