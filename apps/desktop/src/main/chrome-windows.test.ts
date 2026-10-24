import { execFile } from 'node:child_process'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  registerWindowsChromeHost,
  secureWindowsChromeDirectory,
  windowsChromeHostRegistered,
  windowsNativeHostPath,
} from './chrome-windows'

vi.mock('node:child_process', () => ({ execFile: vi.fn() }))

describe('Windows Chrome system integration', () => {
  const path = String.raw`C:\Users\测试 & 100%\AppData\Roaming\Open Harness\chrome-use\host.json`
  beforeEach(() => {
    vi.mocked(execFile).mockReset()
    vi.mocked(execFile).mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (
        error: Error | null,
        result: { stdout: string; stderr: string },
      ) => void
      // execFile's real promisify hook resolves the pair as an object.
      callback(null, { stdout: Buffer.from(path, 'utf8').toString('base64'), stderr: '' })
      return {} as ReturnType<typeof execFile>
    })
  })

  it('registers both Chrome registry views without a shell or elevation', async () => {
    await registerWindowsChromeHost(path)
    expect(vi.mocked(execFile).mock.calls).toHaveLength(2)
    for (const [index, call] of vi.mocked(execFile).mock.calls.entries()) {
      expect(call[0]).toMatch(/\\System32\\reg\.exe$/)
      expect(call[1]).toEqual([
        'ADD',
        'HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\dev.openharness.chrome',
        '/ve',
        '/t',
        'REG_SZ',
        '/d',
        path,
        '/f',
        `/reg:${index === 0 ? 32 : 64}`,
      ])
      expect(call[2]).toMatchObject({ windowsHide: true, timeout: 10_000 })
      expect(call[2]).not.toHaveProperty('shell')
    }
  })

  it('preserves Unicode registry paths and rejects a different host', async () => {
    expect(await windowsChromeHostRegistered(path.toUpperCase())).toBe(true)
    expect(await windowsChromeHostRegistered(String.raw`C:\other\host.json`)).toBe(false)
    for (const call of vi.mocked(execFile).mock.calls) {
      const script = Buffer.from((call[1] as string[]).at(-1)!, 'base64').toString('utf16le')
      expect(script).toContain('OpenSubKey')
      expect(script).not.toContain('SetValue')
    }
  })

  it('keeps paths separate from the fixed ACL script', async () => {
    await secureWindowsChromeDirectory(path)
    const call = vi.mocked(execFile).mock.calls[0]
    const args = call[1] as string[]
    const script = Buffer.from(args.at(-1)!, 'base64').toString('utf16le')
    expect(script).not.toContain(path)
    expect(script).toContain('SetAccessRuleProtection($true, $false)')
    expect(script).toContain('$item.SetAccessControl($acl)')
    expect(script).not.toMatch(/\b(?:Get-Item|Set-Acl)\b/)
    expect(script).toContain("SecurityIdentifier]::new('S-1-5-18')")
    expect(call[2]).toMatchObject({
      env: { OPENHARNESS_CHROME_PRIVATE_DIRECTORY: path },
      windowsHide: true,
    })
  })

  it('keeps ordinary native paths and resolves percent paths without a shell', async () => {
    const ordinary = String.raw`C:\Users\测试 & user\chrome-use\native-host.exe`
    expect(await windowsNativeHostPath(ordinary)).toBe(ordinary)
    expect(execFile).not.toHaveBeenCalled()
    const launcher = String.raw`C:\Users\%PATH%\chrome-use\native-host.exe`
    const alias = String.raw`C:\Users\USER~1\CHROME~1\NATIVE~1.EXE`
    vi.mocked(execFile).mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (error: null, result: { stdout: string }) => void
      callback(null, { stdout: JSON.stringify(alias) })
      return {} as ReturnType<typeof execFile>
    })
    expect(await windowsNativeHostPath(launcher)).toBe(alias)
    const call = vi.mocked(execFile).mock.calls[0]
    expect(call[0]).toBe(launcher)
    expect(call[1]).toEqual(['--native-path', launcher])
    expect(call[2]).not.toHaveProperty('shell')
  })

  it('rejects an alias that Chrome would still expand or cannot execute', async () => {
    for (const value of [
      String.raw`C:\%PATH%\host.exe`,
      String.raw`C:\!PATH!\host.exe`,
      'host.exe',
      null,
    ]) {
      vi.mocked(execFile).mockImplementation((...args: unknown[]) => {
        const callback = args.at(-1) as (error: null, result: { stdout: string }) => void
        callback(null, { stdout: JSON.stringify(value) })
        return {} as ReturnType<typeof execFile>
      })
      await expect(windowsNativeHostPath(String.raw`C:\%PATH%\host.exe`)).rejects.toThrow(
        'Chrome 无法从当前目录启动连接组件',
      )
    }
  })
})
