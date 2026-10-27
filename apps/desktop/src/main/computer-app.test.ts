import { execFile } from 'node:child_process'
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  stat,
  symlink,
  writeFile,
} from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { COMPUTER_APP_NAME, installComputerApp } from './computer-app'

vi.mock('node:child_process', () => ({ execFile: vi.fn() }))
vi.mock('node:fs/promises', async (importOriginal) => {
  const original = await importOriginal<typeof import('node:fs/promises')>()
  return { ...original, rename: vi.fn(original.rename) }
})

describe('independently installed macOS Computer Use app', () => {
  let directory: string
  let source: string
  let userData: string
  let target: string
  const binary = (app: string) => join(app, 'Contents', 'MacOS', 'OpenHarnessComputerUse')
  const succeed = (...args: unknown[]) => {
    const callback = args.at(-1) as (error: Error | null, result?: object) => void
    callback(null, { stdout: '', stderr: 'Identifier=dev.openharness.computeruse\n' })
    return {} as ReturnType<typeof execFile>
  }
  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'oh-computer-install-'))
    source = join(directory, 'Main.app', 'Contents', 'Resources', COMPUTER_APP_NAME)
    userData = join(directory, 'data')
    target = join(directory, 'data', 'computer-use', COMPUTER_APP_NAME)
    await mkdir(join(source, 'Contents', 'MacOS'), { recursive: true })
    await mkdir(join(source, 'Contents', '_CodeSignature'))
    await writeFile(binary(source), 'signed executable fixture', { mode: 0o755 })
    await writeFile(join(source, 'Contents', 'Info.plist'), 'signed plist fixture')
    await writeFile(join(source, 'Contents', '_CodeSignature', 'CodeResources'), 'resource seal')
    vi.mocked(execFile).mockReset().mockImplementation(succeed)
  })
  afterEach(async () => {
    vi.mocked(rename).mockImplementation(
      (await vi.importActual<typeof import('node:fs/promises')>('node:fs/promises')).rename,
    )
    await rm(directory, { recursive: true, force: true })
  })

  it('copies signed bytes to an ordinary stable private location without signing or launching', async () => {
    const installed = await installComputerApp(source, userData)
    expect(installed).not.toContain('Main.app/')
    target = installed
    expect(await readFile(binary(target))).toEqual(await readFile(binary(source)))
    expect(await readFile(join(target, 'Contents', 'Info.plist'), 'utf8')).toBe(
      'signed plist fixture',
    )
    expect(
      await readFile(join(target, 'Contents', '_CodeSignature', 'CodeResources'), 'utf8'),
    ).toBe('resource seal')
    expect((await stat(binary(target))).mode & 0o777).toBe(0o755)
    expect((await stat(join(target, '..'))).mode & 0o777).toBe(0o700)
    for (const call of vi.mocked(execFile).mock.calls) {
      expect(call[0]).toBe('/usr/bin/codesign')
      expect((call[1] as string[])[0]).toMatch(/^--(?:verify|display)$/)
    }
  })

  it('leaves an unchanged signed installation in place', async () => {
    target = await installComputerApp(source, userData)
    const before = await stat(binary(target))
    expect(await installComputerApp(source, userData)).toBe(target)
    const after = await stat(binary(target))
    expect(after.ino).toBe(before.ino)
    expect(after.mtimeMs).toBe(before.mtimeMs)
  })

  it('replaces a changed bundle atomically and removes old package resources', async () => {
    target = await installComputerApp(source, userData)
    await writeFile(join(target, 'obsolete'), 'old')
    await writeFile(binary(source), 'updated signed executable')
    await installComputerApp(source, userData)
    expect(await readFile(binary(target), 'utf8')).toBe('updated signed executable')
    await expect(stat(join(target, 'obsolete'))).rejects.toMatchObject({ code: 'ENOENT' })
    expect(await readdir(join(target, '..'))).toEqual([COMPUTER_APP_NAME])
  })

  it('preserves the previous bundle when staged signature verification fails', async () => {
    target = await installComputerApp(source, userData)
    await writeFile(binary(source), 'invalid replacement')
    vi.mocked(execFile).mockImplementation((...args: unknown[]) => {
      if ((args[1] as string[]).at(-1)?.includes('/.computer-use-')) {
        ;(args.at(-1) as (error: Error) => void)(new Error('invalid staged signature'))
        return {} as ReturnType<typeof execFile>
      }
      return succeed(...args)
    })
    await expect(installComputerApp(source, userData)).rejects.toThrow('invalid staged signature')
    expect(await readFile(binary(target), 'utf8')).toBe('signed executable fixture')
    expect(await readdir(join(target, '..'))).toEqual([COMPUTER_APP_NAME])
  })

  it('restores the previous bundle if publication fails', async () => {
    target = await installComputerApp(source, userData)
    await writeFile(binary(source), 'new signed executable')
    const original = await vi.importActual<typeof import('node:fs/promises')>('node:fs/promises')
    vi.mocked(rename).mockImplementation(async (from, to) => {
      if (to === target && String(from).endsWith(COMPUTER_APP_NAME))
        throw new Error('publish failed')
      await original.rename(from, to)
    })
    await expect(installComputerApp(source, userData)).rejects.toThrow('publish failed')
    expect(await readFile(binary(target), 'utf8')).toBe('signed executable fixture')
  })

  it('rejects a different signature identity before creating the installation', async () => {
    vi.mocked(execFile).mockImplementation((...args: unknown[]) => {
      ;(args.at(-1) as (error: null, result: object) => void)(null, {
        stdout: '',
        stderr: 'Identifier=other.app\n',
      })
      return {} as ReturnType<typeof execFile>
    })
    await expect(installComputerApp(source, userData)).rejects.toThrow('签名身份')
    await expect(stat(userData)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects links in the package without replacing the installed bundle', async () => {
    target = await installComputerApp(source, userData)
    await symlink('/etc/hosts', join(source, 'Contents', 'linked'))
    await expect(installComputerApp(source, userData)).rejects.toThrow('符号链接')
    expect(await readFile(binary(target), 'utf8')).toBe('signed executable fixture')
  })

  it.each(['parent', 'app'])(
    'rejects a linked installation %s without touching its target',
    async (kind) => {
      await mkdir(userData)
      const outside = join(directory, 'outside')
      await mkdir(outside)
      await writeFile(join(outside, 'keep'), 'original')
      if (kind === 'parent') await symlink(outside, join(userData, 'computer-use'))
      else {
        await mkdir(join(userData, 'computer-use'))
        await symlink(outside, target)
      }
      await expect(installComputerApp(source, userData)).rejects.toThrow('符号链接')
      expect(await readdir(outside)).toEqual(['keep'])
    },
  )

  it('refuses a custom data directory nested in another application', async () => {
    await expect(installComputerApp(source, join(directory, 'Other.app', 'data'))).rejects.toThrow(
      '应用包以外',
    )
  })

  it('rejects a non-executable helper', async () => {
    await chmod(binary(source), 0o644)
    await expect(installComputerApp(source, userData)).rejects.toThrow('可执行文件')
  })
})
