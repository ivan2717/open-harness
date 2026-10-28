import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import { dialog, ipcMain, shell, type BrowserWindow, type IpcMainInvokeEvent } from 'electron'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Backend } from './backend'
import type { ResolvedResource } from '../shared/message-resources'
import { installMessageResourceBridge } from './message-resource-bridge'
import { channels } from './protocol'

vi.mock('electron', () => ({
  dialog: { showSaveDialog: vi.fn() },
  ipcMain: { handle: vi.fn() },
  protocol: { handle: vi.fn() },
  shell: { openPath: vi.fn(), showItemInFolder: vi.fn(), openExternal: vi.fn() },
}))

describe('document snapshot resource actions', () => {
  let directory: string
  let name: string
  let text: string
  const handlers = new Map<string, Parameters<typeof ipcMain.handle>[1]>()
  const reference = { runId: 'run-1', messageId: 'event-0', fileIndex: 0 }
  const backend = {
    request: async ({ path }: { path: string }): Promise<unknown> => {
      if (path === '/api/runs/run-1')
        return {
          id: 'run-1',
          messages: [{ id: 'event-0', role: 'user', files: [{ name, size: text.length }] }],
        }
      if (path === '/api/runs/run-1/messages/event-0/files/0') return { name, text }
      throw new Error('Unexpected resource request')
    },
  } as unknown as Backend

  function install(): void {
    installMessageResourceBridge({
      directory,
      backend,
      window: () => ({}) as BrowserWindow,
      assertSender: vi.fn(),
    })
  }

  async function invoke(channel: string, ...args: unknown[]): Promise<unknown> {
    return handlers.get(channel)!({} as IpcMainInvokeEvent, ...args)
  }

  beforeEach(async () => {
    vi.clearAllMocks()
    handlers.clear()
    directory = await mkdtemp(join(tmpdir(), 'openharness-resource-actions-'))
    name = 'source.pdf'
    text = 'Extraction warning\n[page 1]\nSynthetic captured document text'
    vi.mocked(ipcMain.handle).mockImplementation((channel, listener) => {
      handlers.set(channel, listener)
    })
    vi.mocked(shell.openPath).mockResolvedValue('')
    install()
  })

  afterEach(async () => {
    const exports = new Set(vi.mocked(shell.openPath).mock.calls.map(([path]) => dirname(path)))
    for (const path of exports) await rm(path, { recursive: true, force: true })
    await rm(directory, { recursive: true, force: true })
  })

  it.each(['pdf', 'docx', 'xlsx', 'pptx'])(
    'opens and saves attached %s text under a .txt filename after cache reload',
    async (extension) => {
      name = `source.${extension}`
      const capturedText = text
      const first = (await invoke(channels.resolveResource, reference)) as ResolvedResource
      expect(first.name).toBe(name)
      expect(first.kind).toBe('text')
      expect(first.text).toBe(capturedText)
      await invoke(channels.resourceAction, first.id, 'open')
      const opened = vi.mocked(shell.openPath).mock.calls[0][0]
      expect(basename(opened)).toBe(`${name}.txt`)
      expect(await readFile(opened, 'utf8')).toBe(capturedText)

      text = 'Changed backend text must not replace the saved snapshot'
      install()
      const restored = (await invoke(channels.resolveResource, reference)) as ResolvedResource
      expect(restored).toEqual(first)
      const saved = join(directory, `${name}.txt`)
      vi.mocked(dialog.showSaveDialog).mockResolvedValue({ canceled: false, filePath: saved })
      await invoke(channels.resourceAction, restored.id, 'save')
      expect(dialog.showSaveDialog).toHaveBeenCalledWith(
        {},
        expect.objectContaining({ defaultPath: `${name}.txt` }),
      )
      expect(await readFile(saved, 'utf8')).toBe(capturedText)
    },
  )
})
