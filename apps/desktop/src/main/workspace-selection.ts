import { randomUUID } from 'node:crypto'
import { mkdir, readFile, realpath, rename, rm, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute } from 'node:path'
import type { OpenDialogOptions, OpenDialogReturnValue } from 'electron'

function workspacePath(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !isAbsolute(value) ||
    value.length > 4096 ||
    /[\u0000-\u001f\u007f]/.test(value)
  )
    throw new Error('项目路径无效，请重新选择项目目录。')
  return value
}

async function directoryPath(path: string): Promise<string> {
  try {
    const canonical = await realpath(path)
    if (!(await stat(canonical)).isDirectory()) throw new Error('Not a directory')
    return canonical
  } catch {
    throw new Error('无法打开此项目目录，请确认目录仍然存在且可以访问，然后重新选择。')
  }
}

async function unchangedDirectory(path: string): Promise<void> {
  if ((await directoryPath(path)) !== path)
    throw new Error('项目目录的实际位置已改变，请通过“添加项目”重新选择目录。')
}

type WorkspaceHistory = () => Promise<readonly { workspace_path?: string | null }[]>
type WorkspacePicker = (options: OpenDialogOptions) => Promise<OpenDialogReturnValue>

/** Only native selections and trusted runtime history establish known projects. */
export class WorkspaceSelection {
  private known = new Set<string>()
  private selected = new Set<string>()
  private loaded = false
  private pending: Promise<unknown> = Promise.resolve()

  constructor(
    private file: string,
    private history: WorkspaceHistory,
  ) {}

  async choose(picker: WorkspacePicker): Promise<string | null> {
    const result = await picker({
      title: '选择项目目录',
      buttonLabel: '选择项目',
      properties: ['openDirectory'],
    })
    if (result.canceled || !result.filePaths.length) return null
    if (result.filePaths.length !== 1) throw new Error('请选择一个项目目录。')
    return this.serialize(async () => {
      const path = await directoryPath(workspacePath(result.filePaths[0]))
      await this.load()
      await this.remember(path)
      this.selected.add(path)
      return path
    })
  }

  /** Reopening a known project never invokes a directory picker. */
  open(value: unknown): Promise<string> {
    return this.serialize(async () => {
      const path = workspacePath(value)
      await this.load()
      if (!this.known.has(path)) {
        const runs = await this.history()
        if (!runs.some((run) => run.workspace_path === path))
          throw new Error('尚未添加此项目，请通过“添加项目”或“选择项目”选择目录。')
      }
      await unchangedDirectory(path)
      await this.remember(path)
      this.selected.add(path)
      return path
    })
  }

  /** Starting work requires an explicit selection in this app session. */
  verify(value: unknown): Promise<void> {
    return this.serialize(async () => {
      const path = workspacePath(value)
      if (!this.selected.has(path))
        throw new Error('请先通过“选择项目”或项目的新建会话按钮打开此目录。')
      try {
        await unchangedDirectory(path)
      } catch (error) {
        this.selected.delete(path)
        throw error
      }
    })
  }

  private serialize<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.pending.then(operation, operation)
    this.pending = result.catch(() => undefined)
    return result
  }

  private async load(): Promise<void> {
    if (this.loaded) return
    try {
      let source: string
      try {
        source = await readFile(this.file, 'utf8')
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
        this.loaded = true
        return
      }
      const saved: unknown = JSON.parse(source)
      if (
        !saved ||
        typeof saved !== 'object' ||
        !('version' in saved) ||
        saved.version !== 1 ||
        !('paths' in saved) ||
        !Array.isArray(saved.paths)
      )
        throw new Error('Invalid workspace registry')
      this.known = new Set(saved.paths.map(workspacePath))
      this.loaded = true
    } catch {
      // Do not overwrite an unreadable registry with a partial list.
      throw new Error('无法读取已保存的项目记录，请检查应用数据目录是否可访问。')
    }
  }

  private async remember(path: string): Promise<void> {
    if (this.known.has(path)) return
    const paths = new Set([...this.known, path])
    const temporary = `${this.file}.${randomUUID()}.tmp`
    try {
      await mkdir(dirname(this.file), { recursive: true, mode: 0o700 })
      await writeFile(temporary, JSON.stringify({ version: 1, paths: [...paths] }), {
        mode: 0o600,
        flag: 'wx',
      })
      await rename(temporary, this.file)
      this.known = paths
    } catch {
      throw new Error('无法保存项目记录，请检查应用数据目录是否可以写入。')
    } finally {
      await rm(temporary, { force: true }).catch(() => undefined)
    }
  }
}
