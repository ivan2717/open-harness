import { execFile } from 'node:child_process'
import { win32 } from 'node:path'
import { promisify } from 'node:util'
import { backendSandboxEnvironment } from './backend'
import type { WindowsScriptEnvironmentStatus } from '../shared/windows-script-environment'

const execute = promisify(execFile)

// The renderer cannot select a program, command, path or elevation arguments.
// Only this fixed preparation operation runs elevated; the app and scripts do not.
const preparationScript = `
$ErrorActionPreference = 'Stop'
try {
  $process = Start-Process -FilePath $env:OPENHARNESS_METADATA_HELPER -ArgumentList '--prepare-metadata' -Verb RunAs -Wait -PassThru
  @{ outcome = 'completed'; exit_code = $process.ExitCode } | ConvertTo-Json -Compress
} catch {
  $cause = $_.Exception
  while ($null -ne $cause) {
    if ($cause -is [ComponentModel.Win32Exception] -and $cause.NativeErrorCode -eq 1223) {
      @{ outcome = 'cancelled' } | ConvertTo-Json -Compress
      exit 0
    }
    $cause = $cause.InnerException
  }
  throw
}
`

function statusFromOutput(output: string): WindowsScriptEnvironmentStatus {
  const value: unknown = JSON.parse(output)
  if (!value || typeof value !== 'object') throw new Error('Invalid script environment status')
  const status = value as Partial<WindowsScriptEnvironmentStatus>
  const pathsValid = (paths: unknown): paths is string[] =>
    Array.isArray(paths) &&
    paths.length <= 128 &&
    paths.every((path) => typeof path === 'string' && win32.isAbsolute(path))
  if (
    typeof status.ready !== 'boolean' ||
    !pathsValid(status.paths) ||
    !pathsValid(status.missing_paths) ||
    status.missing_paths.some((path) => !status.paths!.includes(path)) ||
    status.ready !== (status.missing_paths.length === 0)
  )
    throw new Error('Invalid script environment status')
  return { ready: status.ready, paths: status.paths, missing_paths: status.missing_paths }
}

export class WindowsScriptEnvironment {
  private readonly helper: string
  private preparing: Promise<WindowsScriptEnvironmentStatus> | undefined

  constructor(
    binary: string,
    private readonly platform: NodeJS.Platform = process.platform,
  ) {
    this.helper =
      backendSandboxEnvironment(binary, platform).OPENHARNESS_WINDOWS_SANDBOX_HELPER ?? ''
  }

  async status(): Promise<WindowsScriptEnvironmentStatus> {
    if (this.platform !== 'win32') throw new Error('此设置仅适用于 Windows。')
    try {
      const { stdout } = await execute(this.helper, ['--metadata-status'], {
        timeout: 15_000,
        maxBuffer: 64 * 1024,
        windowsHide: true,
        encoding: 'utf8',
      })
      return statusFromOutput(stdout)
    } catch {
      throw new Error('无法检查脚本环境，请重试；若仍失败，请更新或重新安装 Open Harness。')
    }
  }

  prepare(): Promise<WindowsScriptEnvironmentStatus> {
    this.preparing ??= this.prepareOnce().finally(() => {
      this.preparing = undefined
    })
    return this.preparing
  }

  private async prepareOnce(): Promise<WindowsScriptEnvironmentStatus> {
    const before = await this.status()
    if (before.ready) return before
    let outcome: unknown
    try {
      const { stdout } = await execute(
        win32.join(
          process.env.SystemRoot ?? 'C:\\Windows',
          'System32',
          'WindowsPowerShell',
          'v1.0',
          'powershell.exe',
        ),
        [
          '-NoLogo',
          '-NoProfile',
          '-NonInteractive',
          '-EncodedCommand',
          Buffer.from(preparationScript, 'utf16le').toString('base64'),
        ],
        {
          env: { ...process.env, OPENHARNESS_METADATA_HELPER: this.helper },
          timeout: 300_000,
          maxBuffer: 16 * 1024,
          windowsHide: true,
          encoding: 'utf8',
        },
      )
      outcome = JSON.parse(stdout)
    } catch {
      throw new Error('脚本环境准备失败或系统确认超时，请重试。')
    }
    if (!outcome || typeof outcome !== 'object') throw new Error('脚本环境准备返回了无效结果。')
    if ('outcome' in outcome && outcome.outcome === 'cancelled')
      throw new Error('已取消系统授权，脚本环境尚未准备。')
    if (
      !('outcome' in outcome) ||
      outcome.outcome !== 'completed' ||
      !('exit_code' in outcome) ||
      outcome.exit_code !== 0
    )
      throw new Error('脚本环境准备未完成，请重新点击准备并允许 Windows 系统确认。')
    const after = await this.status()
    if (!after.ready) throw new Error('系统准备已结束，但脚本环境仍未就绪，请重试。')
    return after
  }
}
