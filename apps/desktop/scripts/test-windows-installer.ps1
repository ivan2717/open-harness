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

function Assert-BackendDependencies {
    # The runner has Visual Studio installed, which can hide a missing runtime
    # on users' machines. Inspect the installed backend instead of trusting that
    # it launches here or relying only on the compiler's static-CRT flag.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $installations = @(& $vswhere -all -products '*' -prerelease -property installationPath)
    if ($LASTEXITCODE -ne 0) { throw 'Could not locate Visual Studio installations' }
    $dumpbin = $installations | Where-Object { $_.Trim() } | ForEach-Object {
        Get-ChildItem (Join-Path ($_.Trim()) 'VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe') -ErrorAction SilentlyContinue
    } | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $dumpbin) { throw "Could not locate dumpbin under: $($installations -join ', ')" }
    $artifacts = if ($env:OPENHARNESS_E2E_ARTIFACTS) {
        [System.IO.Path]::GetFullPath($env:OPENHARNESS_E2E_ARTIFACTS)
    } else { Join-Path $desktop 'test-results/packaged' }
    New-Item -ItemType Directory -Path $artifacts -Force | Out-Null
    foreach ($binary in @('api', 'openharness-chrome-host')) {
        $executable = Join-Path $installDirectory "resources/bin/$binary.exe"
        if (-not (Test-Path $executable)) { throw "Installed binary missing: $binary" }
        $dependencies = (& $dumpbin.FullName /DEPENDENTS $executable 2>&1 | Out-String)
        $dependencyExitCode = $LASTEXITCODE
        Set-Content (Join-Path $artifacts "$binary-dependencies.txt") -Value $dependencies -Encoding utf8
        if ($dependencyExitCode -ne 0 -or $dependencies -notmatch '(?im)^\s*\S+\.dll\s*$') {
            throw "Could not inspect installed $binary dependencies"
        }
        if ($dependencies -match '(?im)^\s*(?:vcruntime|msvcp|msvcr)\d[\w.-]*\.dll\s*$') {
            throw "Installed $binary still requires a separate Visual C++ runtime"
        }
    }
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
    Assert-BackendDependencies
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = (Get-Command node).Source
    $startInfo.UseShellExecute = $false
    $startInfo.ArgumentList.Add((Join-Path $desktop 'tests/e2e/packaged.mjs'))
    $smoke = [System.Diagnostics.Process]::Start($startInfo)
    try {
        if (-not $smoke.WaitForExit(300000)) {
            throw 'Installed app smoke test timed out after 5 minutes; see stage diagnostics'
        }
        if ($smoke.ExitCode -ne 0) { throw 'Installed app smoke test failed' }
    } finally {
        if (-not $smoke.HasExited) {
            # Terminate only this test's process tree, including its isolated
            # Chromium and Electron children, before trying to uninstall.
            & "$env:SystemRoot\System32\taskkill.exe" /PID $smoke.Id /T /F
            $null = $smoke.WaitForExit(10000)
        }
        $smoke.Dispose()
    }
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
