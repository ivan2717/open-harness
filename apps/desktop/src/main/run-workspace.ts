import { lstat, realpath } from 'node:fs/promises'
import { posix, win32 } from 'node:path'
import type { RunSummary } from '../shared/contracts'
import { identifier } from './protocol'

/** Resolve the persisted run binding, never a directory supplied by the renderer. */
export async function revealRunWorkspace(
  value: unknown,
  request: (request: { path: string; method: 'GET' }) => Promise<unknown>,
  openDirectory: (path: string) => Promise<string>,
  platform: NodeJS.Platform = process.platform,
): Promise<void> {
  const id = identifier(value)
  const run = (await request({ path: `/api/runs/${id}`, method: 'GET' })) as RunSummary
  const directory = run.workspace_path ?? run.execution_directory
  const paths = platform === 'win32' ? win32 : posix
  if (!directory || !paths.isAbsolute(directory) || /[\u0000-\u001f\u007f]/.test(directory))
    throw new Error('此会话没有本地工作目录。')
  const metadata = await lstat(directory)
  if (!metadata.isDirectory() || metadata.isSymbolicLink())
    throw new Error('工作目录的位置已改变，请重新打开会话。')
  const canonical = await realpath(directory)
  // Rust canonicalization may retain the Windows namespace prefix while Node
  // returns a DOS/UNC path. Normalize representation, never directory identity.
  if (paths.toNamespacedPath(canonical) !== paths.toNamespacedPath(directory))
    throw new Error('工作目录的位置已改变，请重新打开会话。')
  const error = await openDirectory(canonical)
  if (error) throw new Error('无法打开工作目录，请检查该目录是否仍然可用。')
}
