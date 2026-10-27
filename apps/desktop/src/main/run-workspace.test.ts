import { mkdtemp, mkdir, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { revealRunWorkspace } from './run-workspace'

describe('opening a conversation working directory', () => {
  let directory: string
  beforeEach(async () => {
    directory = await realpath(await mkdtemp(join(tmpdir(), 'openharness-reveal-')))
  })
  afterEach(async () => {
    await rm(directory, { recursive: true, force: true })
  })

  it('gets the directory from the persisted run instead of accepting a renderer path', async () => {
    const request = vi.fn(async () => ({ execution_directory: directory }))
    const open = vi.fn(async () => '')
    await revealRunWorkspace('run-1', request, open)
    expect(request).toHaveBeenCalledWith({ path: '/api/runs/run-1', method: 'GET' })
    expect(open).toHaveBeenCalledWith(directory)
    await expect(revealRunWorkspace('/tmp/arbitrary', request, open)).rejects.toThrow()
    expect(request).toHaveBeenCalledTimes(1)
    expect(open).toHaveBeenCalledTimes(1)
  })

  it('prefers the selected project and refuses links and files substituted for folders', async () => {
    const project = join(directory, 'project')
    await mkdir(project)
    const open = vi.fn(async () => '')
    await revealRunWorkspace(
      'run-1',
      async () => ({ workspace_path: project, execution_directory: directory }),
      open,
    )
    expect(open).toHaveBeenCalledWith(project)
    const alias = join(directory, 'alias')
    await symlink(project, alias)
    await expect(
      revealRunWorkspace('run-1', async () => ({ execution_directory: alias }), open),
    ).rejects.toThrow('位置已改变')
    const file = join(directory, 'file')
    await writeFile(file, 'not a directory')
    await expect(
      revealRunWorkspace('run-1', async () => ({ execution_directory: file }), open),
    ).rejects.toThrow('位置已改变')
    expect(open).toHaveBeenCalledTimes(1)
  })

  it('reports missing bindings and operating system failures', async () => {
    const open = vi.fn(async () => 'access denied')
    await expect(revealRunWorkspace('run-1', async () => ({}), open)).rejects.toThrow(
      '没有本地工作目录',
    )
    expect(open).not.toHaveBeenCalled()
    await expect(
      revealRunWorkspace('run-1', async () => ({ execution_directory: directory }), open),
    ).rejects.toThrow('无法打开工作目录')
  })
})
