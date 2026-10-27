import { beforeEach, describe, expect, it, vi } from 'vitest'
import { revealRunWorkspace } from './run-workspace'

const files = vi.hoisted(() => ({ lstat: vi.fn(), realpath: vi.fn() }))
vi.mock('node:fs/promises', () => files)

describe('Windows conversation directory representations', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    files.lstat.mockResolvedValue({ isDirectory: () => true, isSymbolicLink: () => false })
  })

  it.each([
    ['C:\\Users\\test\\session-files\\run-1', '\\\\?\\C:\\Users\\test\\session-files\\run-1'],
    ['\\\\server\\share\\session-files\\run-1', '\\\\?\\UNC\\server\\share\\session-files\\run-1'],
  ])(
    'opens the same canonical directory for ordinary and namespaced paths: %s',
    async (plain, namespaced) => {
      for (const binding of [plain, namespaced]) {
        files.realpath.mockResolvedValue(plain)
        const open = vi.fn(async () => '')
        await revealRunWorkspace(
          'run-1',
          async () => ({ execution_directory: binding }),
          open,
          'win32',
        )
        expect(files.lstat).toHaveBeenLastCalledWith(binding)
        expect(files.realpath).toHaveBeenLastCalledWith(binding)
        expect(open).toHaveBeenCalledExactlyOnceWith(plain)
      }
    },
  )

  it('still rejects symlinks and a different canonical directory', async () => {
    const binding = '\\\\?\\C:\\Users\\test\\session-files\\run-1'
    const open = vi.fn(async () => '')
    const request = async () => ({ execution_directory: binding })
    files.lstat.mockResolvedValueOnce({ isDirectory: () => true, isSymbolicLink: () => true })
    await expect(revealRunWorkspace('run-1', request, open, 'win32')).rejects.toThrow('位置已改变')
    expect(files.realpath).not.toHaveBeenCalled()

    files.realpath.mockResolvedValue('C:\\Users\\test\\session-files\\another-run')
    await expect(revealRunWorkspace('run-1', request, open, 'win32')).rejects.toThrow('位置已改变')
    expect(open).not.toHaveBeenCalled()
  })
})
