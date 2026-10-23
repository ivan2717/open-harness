import {
  mkdtemp,
  mkdir,
  readFile,
  realpath,
  rename,
  rm,
  stat,
  symlink,
  writeFile,
} from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { WorkspaceSelection } from './workspace-selection'

const directories: string[] = []

async function fixture() {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'openharness-project-choice-')))
  directories.push(directory)
  const project = join(directory, 'project')
  await mkdir(project)
  const file = join(directory, 'data', 'workspaces.json')
  const history = vi.fn().mockResolvedValue([])
  const selection = new WorkspaceSelection(file, history)
  const picker = vi.fn().mockResolvedValue({ canceled: false, filePaths: [project] })
  return { directory, project, file, history, selection, picker }
}

afterEach(async () => {
  await Promise.all(directories.splice(0).map((path) => rm(path, { recursive: true, force: true })))
})

describe('workspace selection', () => {
  it('persists the canonical native selection and reopens without another picker', async () => {
    const { directory, project, file, history, selection, picker } = await fixture()
    const alias = join(directory, 'project alias')
    await symlink(project, alias, process.platform === 'win32' ? 'junction' : 'dir')
    picker.mockResolvedValue({ canceled: false, filePaths: [alias] })

    expect(await selection.choose(picker)).toBe(project)
    expect(picker).toHaveBeenCalledWith({
      title: '选择项目目录',
      buttonLabel: '选择项目',
      properties: ['openDirectory'],
    })
    await expect(selection.verify(project)).resolves.toBeUndefined()
    expect(await selection.open(project)).toBe(project)
    expect(picker).toHaveBeenCalledOnce()
    expect(history).not.toHaveBeenCalled()
    expect(JSON.parse(await readFile(file, 'utf8'))).toEqual({ version: 1, paths: [project] })
    if (process.platform !== 'win32') expect((await stat(file)).mode & 0o777).toBe(0o600)
  })

  it('always opens a picker when explicitly choosing or changing a project', async () => {
    const { selection, picker } = await fixture()
    await selection.choose(picker)
    await selection.choose(picker)
    expect(picker).toHaveBeenCalledTimes(2)
  })

  it('does not grant or persist canceled native selections', async () => {
    const { project, file, selection, picker } = await fixture()
    picker.mockResolvedValue({ canceled: true, filePaths: [project] })
    expect(await selection.choose(picker)).toBeNull()
    await expect(selection.verify(project)).rejects.toThrow('选择项目')
    await expect(selection.open(project)).rejects.toThrow('尚未添加此项目')
    await expect(readFile(file)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('reopens a saved empty project after restart without consulting run history', async () => {
    const { project, file, selection, picker } = await fixture()
    await selection.choose(picker)
    const history = vi.fn().mockRejectedValue(new Error('Runtime unavailable'))
    const restarted = new WorkspaceSelection(file, history)
    await expect(restarted.verify(project)).rejects.toThrow('选择项目')
    expect(await restarted.open(project)).toBe(project)
    await expect(restarted.verify(project)).resolves.toBeUndefined()
    expect(history).not.toHaveBeenCalled()
    expect(picker).toHaveBeenCalledOnce()
  })

  it('migrates a legacy project only from trusted runtime history', async () => {
    const { project, file, history, selection } = await fixture()
    history.mockResolvedValue([{ workspace_path: null }, {}, { workspace_path: project }])
    expect(await selection.open(project)).toBe(project)
    await expect(selection.verify(project)).resolves.toBeUndefined()
    expect(history).toHaveBeenCalledOnce()
    expect(JSON.parse(await readFile(file, 'utf8')).paths).toEqual([project])
    const restarted = new WorkspaceSelection(file, vi.fn().mockResolvedValue([]))
    expect(await restarted.open(project)).toBe(project)
  })

  it('does not accept an arbitrary renderer path as an existing project', async () => {
    const { project, file, history, selection } = await fixture()
    await expect(selection.open(project)).rejects.toThrow('尚未添加此项目')
    await expect(selection.verify(project)).rejects.toThrow('选择项目')
    expect(history).toHaveBeenCalledOnce()
    await expect(readFile(file)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects missing saved projects without selecting a replacement', async () => {
    const { directory, project, file, selection, picker } = await fixture()
    await selection.choose(picker)
    await rename(project, join(directory, 'moved'))
    const restarted = new WorkspaceSelection(file, vi.fn().mockResolvedValue([]))
    await expect(restarted.open(project)).rejects.toThrow('无法打开此项目目录')
    await expect(restarted.verify(project)).rejects.toThrow('选择项目')
    expect(picker).toHaveBeenCalledOnce()
  })

  it('rejects changed symlink targets on reopen and before starting a run', async () => {
    const { directory, project, file, selection, picker } = await fixture()
    await selection.choose(picker)
    const moved = join(directory, 'moved')
    const outside = join(directory, 'other project')
    await mkdir(outside)
    await rename(project, moved)
    await symlink(outside, project, process.platform === 'win32' ? 'junction' : 'dir')
    const restarted = new WorkspaceSelection(file, vi.fn().mockResolvedValue([]))
    await expect(restarted.open(project)).rejects.toThrow('实际位置已改变')
    await expect(selection.verify(project)).rejects.toThrow('实际位置已改变')
    await expect(selection.verify(project)).rejects.toThrow('选择项目')
    await expect(selection.verify(outside)).rejects.toThrow('选择项目')
    expect(picker).toHaveBeenCalledOnce()
  })

  it('rejects invalid paths before consulting trusted history', async () => {
    const { selection, history } = await fixture()
    for (const path of [
      undefined,
      null,
      42,
      {},
      '',
      'relative/project',
      '/tmp/\0project',
      `/${'x'.repeat(4096)}`,
    ]) {
      await expect(selection.open(path)).rejects.toThrow('项目路径无效')
      await expect(selection.verify(path)).rejects.toThrow('项目路径无效')
    }
    expect(history).not.toHaveBeenCalled()
  })

  it('rejects stale and non-directory native results without granting access', async () => {
    const { directory, selection, picker } = await fixture()
    const file = join(directory, 'file.txt')
    await writeFile(file, 'not a directory')
    for (const path of [file, join(directory, 'missing')]) {
      picker.mockResolvedValue({ canceled: false, filePaths: [path] })
      await expect(selection.choose(picker)).rejects.toThrow('无法打开此项目目录')
      await expect(selection.verify(path)).rejects.toThrow('选择项目')
    }
    picker.mockResolvedValue({ canceled: false, filePaths: [directory, file] })
    await expect(selection.choose(picker)).rejects.toThrow('请选择一个项目目录')
  })

  it('serializes concurrent selections without losing saved projects', async () => {
    const { directory, project, file, selection, picker } = await fixture()
    const other = join(directory, 'other')
    await mkdir(other)
    const chooseOther = vi.fn().mockResolvedValue({ canceled: false, filePaths: [other] })
    await Promise.all([selection.choose(picker), selection.choose(chooseOther)])
    expect(JSON.parse(await readFile(file, 'utf8')).paths.sort()).toEqual([project, other].sort())
    const restarted = new WorkspaceSelection(file, vi.fn().mockResolvedValue([]))
    expect(await restarted.open(project)).toBe(project)
    expect(await restarted.open(other)).toBe(other)
  })

  it('preserves unreadable saved data instead of replacing it', async () => {
    const { directory, project, picker } = await fixture()
    const file = join(directory, 'workspaces.json')
    const invalid = '{invalid saved content'
    await writeFile(file, invalid)
    const selection = new WorkspaceSelection(file, vi.fn().mockResolvedValue([]))
    await expect(selection.choose(picker)).rejects.toThrow('无法读取已保存的项目记录')
    await expect(selection.verify(project)).rejects.toThrow('选择项目')
    expect(await readFile(file, 'utf8')).toBe(invalid)
  })

  it('does not grant a project when saving its registration fails', async () => {
    const { directory, project, file, history, selection, picker } = await fixture()
    // First load the absent registry, then make its parent impossible to create.
    await expect(selection.open(project)).rejects.toThrow('尚未添加此项目')
    await writeFile(join(directory, 'data'), 'not a directory')
    await expect(selection.choose(picker)).rejects.toThrow('无法保存项目记录')
    await expect(selection.verify(project)).rejects.toThrow('选择项目')
    expect(history).toHaveBeenCalledOnce()
    await expect(readFile(file)).rejects.toMatchObject({ code: 'ENOTDIR' })
  })
})
