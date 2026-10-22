import {
  dialog,
  ipcMain,
  protocol,
  shell,
  type BrowserWindow,
  type IpcMainInvokeEvent,
} from 'electron'
import { copyFile, lstat, mkdtemp } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import type { Backend } from './backend'
import { channels } from './protocol'
import { externalUrl, MessageResourceStore, RESOURCE_SCHEME } from './message-resources'

export function registerMessageResourceScheme(): void {
  protocol.registerSchemesAsPrivileged([
    {
      scheme: RESOURCE_SCHEME,
      privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true },
    },
  ])
}

export function installMessageResourceBridge({
  directory,
  backend,
  window,
  assertSender,
}: {
  directory: string
  backend: Backend
  window: () => BrowserWindow
  assertSender: (event: IpcMainInvokeEvent) => void
}): void {
  const store = new MessageResourceStore(join(directory, 'message-resources'), backend)
  protocol.handle(RESOURCE_SCHEME, async (request) => {
    try {
      return await store.response(request)
    } catch {
      return new Response(null, { status: 404 })
    }
  })
  ipcMain.handle(channels.resolveResource, async (event, reference: unknown) => {
    assertSender(event)
    return store.resolve(reference)
  })
  ipcMain.handle(channels.resourceAction, async (event, id: unknown, action: unknown) => {
    assertSender(event)
    const resource = store.resource(id)
    const archived = store.archivePath(id)
    const exportCopy = async (): Promise<string> => {
      const folder = await mkdtemp(join(tmpdir(), 'openharness-preview-'))
      const path = join(folder, resource.name)
      await copyFile(archived, path)
      return path
    }
    if (action === 'open') {
      if (!resource.canOpen) throw new Error('此格式可预览或另存为；不会直接运行脚本或可执行文件。')
      // Editors must never modify the immutable file retained with the message.
      const error = await shell.openPath(await exportCopy())
      if (error) throw new Error('系统无法打开此文件，请尝试另存为。')
    } else if (action === 'reveal') {
      let path: string | undefined
      if (resource.sourcePath) {
        try {
          if ((await lstat(resource.sourcePath)).isFile()) path = resource.sourcePath
        } catch {
          /* Show the saved snapshot when its source was removed. */
        }
      }
      shell.showItemInFolder(path ?? (await exportCopy()))
    } else if (action === 'save') {
      const result = await dialog.showSaveDialog(window(), {
        title: '另存为',
        defaultPath: resource.name,
        buttonLabel: '保存',
      })
      if (!result.canceled && result.filePath) await copyFile(archived, result.filePath)
    } else throw new Error('文件操作无效。')
  })
  ipcMain.handle(channels.openExternal, async (event, url: unknown) => {
    assertSender(event)
    await shell.openExternal(externalUrl(url))
  })
}
