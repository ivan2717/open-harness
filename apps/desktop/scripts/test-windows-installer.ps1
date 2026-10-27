$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This installer smoke test requires Windows' }

$desktop = Split-Path $PSScriptRoot -Parent
$installers = @(Get-ChildItem (Join-Path $desktop 'release/Open-Harness-*-windows-x64-setup.exe'))
if ($installers.Count -ne 1) { throw 'Expected exactly one Windows installer' }
$installDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("Open Harness Smoke " + [guid]::NewGuid())
$previousExecutable = $env:OPENHARNESS_E2E_EXECUTABLE
$previousArtifacts = $env:OPENHARNESS_E2E_ARTIFACTS

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
    foreach ($binary in @('api', 'openharness-chrome-host', 'openharness-sandbox')) {
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

function Assert-ManagedPython {
    # Exercise the installed copy, not the runner's preinstalled Python. Import
    # checks cover the native wheels and DLLs that a developer machine can hide.
    $runtimeDirectory = Join-Path $installDirectory 'resources/workspace-runtime/python'
    $python = Join-Path $runtimeDirectory 'python.exe'
    if (-not (Test-Path $python)) { throw 'Installed managed Python is missing' }
    $probe = @'
import importlib,json,pathlib,sys,sysconfig
for name in ['ssl','sqlite3','venv','PIL','docx','pptx','openpyxl','pypdf','reportlab']:
    importlib.import_module(name)
assert sys.version_info >= (3,12)
assert (pathlib.Path(sysconfig.get_path('stdlib')) / 'EXTERNALLY-MANAGED').is_file()
print(json.dumps({'version':sys.version,'executable':sys.executable,'prefix':sys.prefix}))
'@
    $result = & $python -I -B -c $probe
    if ($LASTEXITCODE -ne 0) { throw 'Installed Python or preinstalled libraries failed to load' }
    $runtime = $result | ConvertFrom-Json
    if (-not $runtime.prefix.StartsWith($runtimeDirectory, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Managed Python resolved outside the installed application'
    }
    $artifactDirectory = Join-Path $desktop 'test-results/packaged'
    New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null
    $result | Set-Content (Join-Path $artifactDirectory 'python-runtime.json') -Encoding utf8
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
    Assert-ManagedPython
    foreach ($suite in @('packaged', 'script-workflow')) {
        $env:OPENHARNESS_E2E_ARTIFACTS = if ($suite -eq 'script-workflow') {
            Join-Path $desktop 'test-results/packaged/scripts'
        } else { $previousArtifacts }
        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = (Get-Command node).Source
        $startInfo.UseShellExecute = $false
        $startInfo.ArgumentList.Add((Join-Path $desktop "tests/e2e/$suite.mjs"))
        $smoke = [System.Diagnostics.Process]::Start($startInfo)
        try {
            if (-not $smoke.WaitForExit(720000)) {
                throw "Installed app $suite test timed out after 12 minutes; see stage diagnostics"
            }
            if ($smoke.ExitCode -ne 0) { throw "Installed app $suite test failed" }
        } finally {
            if (-not $smoke.HasExited) {
                # Terminate only this test's process tree before uninstalling.
                & "$env:SystemRoot\System32\taskkill.exe" /PID $smoke.Id /T /F
                $null = $smoke.WaitForExit(10000)
            }
            $smoke.Dispose()
        }
    }
} finally {
    $env:OPENHARNESS_E2E_EXECUTABLE = $previousExecutable
    $env:OPENHARNESS_E2E_ARTIFACTS = $previousArtifacts
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
