$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This installer smoke test requires Windows' }

$desktop = Split-Path $PSScriptRoot -Parent
$installers = @(Get-ChildItem (Join-Path $desktop 'release/Open-Harness-*-windows-x64-setup.exe'))
if ($installers.Count -ne 1) { throw 'Expected exactly one Windows installer' }
$installDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("Open Harness Smoke " + [guid]::NewGuid())
$previousExecutable = $env:OPENHARNESS_E2E_EXECUTABLE

function Wait-Installer($process) {
    if (-not $process.WaitForExit(120000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        throw 'Installer or uninstaller timed out'
    }
    if ($process.ExitCode -ne 0) { throw "Installer or uninstaller failed: $($process.ExitCode)" }
}

try {
    # NSIS requires /D last, without quotes, even when the path contains spaces.
    $installer = Start-Process -FilePath $installers[0].FullName -ArgumentList "/S /currentuser /D=$installDirectory" -PassThru
    Wait-Installer $installer
    $env:OPENHARNESS_E2E_EXECUTABLE = Join-Path $installDirectory 'Open Harness.exe'
    if (-not (Test-Path $env:OPENHARNESS_E2E_EXECUTABLE)) { throw 'Installed executable missing' }
    if (-not (Test-Path (Join-Path $installDirectory 'Uninstall Open Harness.exe'))) {
        throw 'Installed uninstaller missing'
    }
    & node (Join-Path $desktop 'tests/e2e/packaged.mjs')
    if ($LASTEXITCODE -ne 0) { throw 'Installed app smoke test failed' }
} finally {
    $env:OPENHARNESS_E2E_EXECUTABLE = $previousExecutable
    $uninstaller = Join-Path $installDirectory 'Uninstall Open Harness.exe'
    if (Test-Path $uninstaller) {
        # Keep the uninstaller in this process so we can wait for real completion.
        $process = Start-Process -FilePath $uninstaller -ArgumentList "/S /currentuser _?=$installDirectory" -PassThru
        Wait-Installer $process
        if (Test-Path (Join-Path $installDirectory 'Open Harness.exe')) {
            throw 'Uninstaller left the application executable behind'
        }
    }
    if (Test-Path $installDirectory) { Remove-Item $installDirectory -Recurse -Force }
}
