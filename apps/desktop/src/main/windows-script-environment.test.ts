import { execFile } from 'node:child_process'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { WindowsScriptEnvironment } from './windows-script-environment'

vi.mock('node:child_process', async (importOriginal) => ({
  ...(await importOriginal<typeof import('node:child_process')>()),
  execFile: vi.fn(),
}))

const binary = String.raw`C:\Users\测试 & O'Neil %PATH% $test\Open Harness\bin\api.exe`
const helper = binary.replace('api.exe', 'openharness-sandbox.exe')
const ready = { ready: true, paths: ['C:\\'], missing_paths: [] }
const missing = { ready: false, paths: ['C:\\'], missing_paths: ['C:\\'] }

function reply(value: unknown) {
  vi.mocked(execFile).mockImplementationOnce((...args: unknown[]) => {
    const callback = args.at(-1) as (
      error: null,
      result: { stdout: string; stderr: string },
    ) => void
    callback(null, { stdout: JSON.stringify(value), stderr: '' })
    return {} as ReturnType<typeof execFile>
  })
}

beforeEach(() => vi.mocked(execFile).mockReset())

describe('Windows script environment setup', () => {
  it('checks the API sibling without elevation or shell resolution', async () => {
    reply(missing)
    await expect(new WindowsScriptEnvironment(binary, 'win32').status()).resolves.toEqual(missing)
    expect(execFile).toHaveBeenCalledExactlyOnceWith(
      helper,
      ['--metadata-status'],
      {
        timeout: 15_000,
        maxBuffer: 64 * 1024,
        windowsHide: true,
        encoding: 'utf8',
      },
      expect.any(Function),
    )
  })

  it('does not invoke UAC when the environment is already ready', async () => {
    reply(ready)
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).resolves.toEqual(ready)
    expect(execFile).toHaveBeenCalledTimes(1)
  })

  it('elevates only fixed preparation, preserves unusual paths, and rechecks as the user', async () => {
    reply(missing)
    reply({ outcome: 'completed', exit_code: 0 })
    reply(ready)
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).resolves.toEqual(ready)
    expect(execFile).toHaveBeenCalledTimes(3)
    const [program, args, options] = vi.mocked(execFile).mock.calls[1]
    expect(program).toMatch(/\\System32\\WindowsPowerShell\\v1\.0\\powershell\.exe$/)
    expect((args as string[]).slice(0, 4)).toEqual([
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-EncodedCommand',
    ])
    const script = Buffer.from((args as string[])[4], 'base64').toString('utf16le')
    expect(script).toContain("-ArgumentList '--prepare-metadata' -Verb RunAs -Wait -PassThru")
    expect(script).toContain('-FilePath $env:OPENHARNESS_METADATA_HELPER')
    expect(script).toContain('NativeErrorCode -eq 1223')
    expect(script).not.toContain(helper)
    expect(script).not.toContain(binary)
    expect(options).toMatchObject({ env: { OPENHARNESS_METADATA_HELPER: helper } })
    expect(options).not.toHaveProperty('shell')
    expect(vi.mocked(execFile).mock.calls[2].slice(0, 2)).toEqual([helper, ['--metadata-status']])
  })

  it('reports UAC cancellation without claiming setup succeeded or retrying elevation', async () => {
    reply(missing)
    reply({ outcome: 'cancelled' })
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).rejects.toThrow(
      '已取消系统授权',
    )
    expect(execFile).toHaveBeenCalledTimes(2)
  })

  it('does not accept helper failure or a still-unprepared environment as success', async () => {
    reply(missing)
    reply({ outcome: 'completed', exit_code: 1 })
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).rejects.toThrow(
      '准备未完成',
    )
    reply(missing)
    reply({ outcome: 'completed', exit_code: 0 })
    reply(missing)
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).rejects.toThrow(
      '仍未就绪',
    )
  })

  it('coalesces repeat clicks into one system confirmation and allows retry after cancellation', async () => {
    const environment = new WindowsScriptEnvironment(binary, 'win32')
    reply(missing)
    reply({ outcome: 'cancelled' })
    const first = environment.prepare()
    expect(environment.prepare()).toBe(first)
    await expect(first).rejects.toThrow('已取消')
    reply(missing)
    reply({ outcome: 'completed', exit_code: 0 })
    reply(ready)
    await expect(environment.prepare()).resolves.toEqual(ready)
    expect(execFile).toHaveBeenCalledTimes(5)
  })

  it.each([
    null,
    { ...ready, ready: 'yes' },
    { ...missing, ready: true },
    { ...missing, missing_paths: ['D:\\'] },
    { ready: true, paths: ['relative'], missing_paths: [] },
  ])('fails closed on malformed metadata status %j', async (value) => {
    reply(value)
    await expect(new WindowsScriptEnvironment(binary, 'win32').prepare()).rejects.toThrow(
      '无法检查',
    )
    expect(execFile).toHaveBeenCalledTimes(1)
  })

  it('rejects non-Windows operations and relative executable paths', async () => {
    await expect(new WindowsScriptEnvironment('/app/api', 'darwin').prepare()).rejects.toThrow(
      '仅适用于 Windows',
    )
    expect(() => new WindowsScriptEnvironment('api.exe', 'win32')).toThrow('must be absolute')
    expect(execFile).not.toHaveBeenCalled()
  })
})
