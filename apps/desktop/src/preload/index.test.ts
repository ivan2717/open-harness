import { beforeEach, describe, expect, it, vi } from 'vitest'
import { contextBridge, ipcRenderer, webUtils } from 'electron'
import type { DesktopBridge } from '../shared/contracts'
import { channels } from '../main/protocol'
import { MAX_IMAGE_SOURCE_BYTES } from '../shared/file-references'
import './index'

vi.mock('electron', () => ({
  contextBridge: { exposeInMainWorld: vi.fn() },
  ipcRenderer: { invoke: vi.fn() },
  webUtils: { getPathForFile: vi.fn() },
}))

const bridge = vi.mocked(contextBridge.exposeInMainWorld).mock.calls[0][1] as DesktopBridge
beforeEach(() => {
  vi.mocked(ipcRenderer.invoke).mockReset()
  vi.mocked(webUtils.getPathForFile).mockReset()
})

describe('file paste bridge', () => {
  it('sends screenshot bytes without asking Electron for a nonexistent file path', async () => {
    const png = new File([new Uint8Array([1, 2, 3])], 'screenshot.png', { type: 'image/png' })
    const result = {
      files: [{ name: 'screenshot.png', image: { mime_type: 'image/png', data_base64: 'AQID' } }],
      errors: [],
    }
    vi.mocked(ipcRenderer.invoke).mockResolvedValue(result)
    expect(await bridge.pasteFiles([png])).toEqual(result)
    expect(webUtils.getPathForFile).not.toHaveBeenCalled()
    expect(ipcRenderer.invoke).toHaveBeenCalledExactlyOnceWith(channels.importImages, [
      { name: 'screenshot.png', bytes: new Uint8Array([1, 2, 3]) },
    ])
  })

  it('prefers native copied file URLs and keeps the path fallback for file drops', async () => {
    const file = new File(['document'], 'notes.pdf', { type: 'application/pdf' })
    const result = { files: [{ name: 'notes.pdf', text: 'document' }], errors: [] }
    vi.mocked(ipcRenderer.invoke).mockResolvedValueOnce(result)
    expect(await bridge.pasteFiles([file])).toEqual(result)
    expect(webUtils.getPathForFile).not.toHaveBeenCalled()
    vi.mocked(ipcRenderer.invoke)
      .mockResolvedValueOnce({ files: [], errors: [] })
      .mockResolvedValueOnce(result)
    vi.mocked(webUtils.getPathForFile).mockReturnValue('/tmp/notes.pdf')
    expect(await bridge.pasteFiles([file])).toEqual(result)
    expect(ipcRenderer.invoke).toHaveBeenLastCalledWith(channels.importFiles, ['/tmp/notes.pdf'])
  })

  it('retains non-image files copied alongside an image', async () => {
    const image = new File(['image'], 'screen.png', { type: 'image/png' })
    const document = new File(['notes'], 'notes.txt', { type: 'text/plain' })
    const imageReference = {
      name: 'screen.png',
      image: { mime_type: 'image/png', data_base64: 'aW1hZ2U=' },
    }
    const textReference = { name: 'notes.txt', text: 'notes' }
    vi.mocked(ipcRenderer.invoke)
      .mockResolvedValueOnce({ files: [imageReference], errors: [] })
      .mockResolvedValueOnce({ files: [], errors: [] })
      .mockResolvedValueOnce({ files: [textReference], errors: [] })
    vi.mocked(webUtils.getPathForFile).mockReturnValue('/tmp/notes.txt')
    expect(await bridge.pasteFiles([image, document])).toEqual({
      files: [imageReference, textReference],
      errors: [],
    })
    expect(webUtils.getPathForFile).toHaveBeenCalledExactlyOnceWith(document)
  })

  it('uses native paths for a document copied with a screenshot without duplicating the image', async () => {
    const image = new File(['image'], 'screen.png', { type: 'image/png' })
    const document = new File(['PDF'], 'notes.pdf', { type: 'application/pdf' })
    const imageReference = {
      name: 'screen.png',
      image: { mime_type: 'image/png', data_base64: 'aW1hZ2U=' },
    }
    const documentReference = { name: 'notes.pdf', path: '/tmp/notes.pdf', text: 'extracted PDF' }
    vi.mocked(ipcRenderer.invoke)
      .mockResolvedValueOnce({ files: [imageReference], errors: [] })
      .mockResolvedValueOnce({ files: [documentReference], errors: [] })
    vi.mocked(webUtils.getPathForFile).mockReturnValue('')
    expect(await bridge.pasteFiles([image, document])).toEqual({
      files: [imageReference, documentReference],
      errors: [],
    })
    expect(webUtils.getPathForFile).not.toHaveBeenCalled()
    expect(ipcRenderer.invoke).toHaveBeenLastCalledWith(channels.pasteFiles, {
      files: [
        { name: 'screen.png', size: 5, image: true },
        { name: 'notes.pdf', size: 3, image: false },
      ],
      skipImages: true,
    })
  })

  it('keeps readable images and documents when other images are empty, too large or unreadable', async () => {
    const empty = new File([], 'empty.png', { type: 'image/png' })
    const large = new File(['image'], 'large.png', { type: 'image/png' })
    Object.defineProperty(large, 'size', { value: MAX_IMAGE_SOURCE_BYTES + 1 })
    const largeRead = vi.spyOn(large, 'arrayBuffer')
    const broken = new File(['image'], 'broken.png', { type: 'image/png' })
    vi.spyOn(broken, 'arrayBuffer').mockRejectedValue(new Error('Fixture read failure'))
    const good = new File(['image'], 'good.png', { type: 'image/png' })
    const document = new File(['PDF'], 'notes.pdf', { type: 'application/pdf' })
    const imageReference = {
      name: 'good.png',
      image: { mime_type: 'image/png', data_base64: 'aW1hZ2U=' },
    }
    const documentReference = { name: 'notes.pdf', text: 'extracted PDF' }
    vi.mocked(ipcRenderer.invoke)
      .mockResolvedValueOnce({ files: [imageReference], errors: [] })
      .mockResolvedValueOnce({ files: [documentReference], errors: [] })
    const result = await bridge.pasteFiles([empty, large, broken, good, document])
    expect(result.files).toEqual([imageReference, documentReference])
    expect(result.errors).toHaveLength(3)
    expect(result.errors[0]).toContain('empty.png')
    expect(result.errors[1]).toContain('large.png')
    expect(result.errors[2]).toContain('broken.png')
    expect(largeRead).not.toHaveBeenCalled()
    expect(ipcRenderer.invoke).toHaveBeenNthCalledWith(1, channels.importImages, [
      { name: 'good.png', bytes: new Uint8Array(await good.arrayBuffer()) },
    ])
    expect(webUtils.getPathForFile).not.toHaveBeenCalled()
  })

  it('still imports a document when no pasted image can be read', async () => {
    const image = new File([], 'empty.png', { type: 'image/png' })
    const document = new File(['PDF'], 'notes.pdf', { type: 'application/pdf' })
    const documentReference = { name: 'notes.pdf', text: 'extracted PDF' }
    vi.mocked(ipcRenderer.invoke).mockResolvedValueOnce({ files: [documentReference], errors: [] })
    const result = await bridge.pasteFiles([image, document])
    expect(result.files).toEqual([documentReference])
    expect(result.errors).toHaveLength(1)
    expect(ipcRenderer.invoke).toHaveBeenCalledOnce()
    expect(ipcRenderer.invoke).toHaveBeenCalledWith(channels.pasteFiles, expect.any(Object))
  })
})

describe('control approval bridge', () => {
  it('keeps allow, always and cancel distinct across the isolated preload boundary', async () => {
    for (const decision of ['allow', 'always', 'cancel'] as const) {
      await bridge.approveExtension('run-1', 'mcp-2', decision)
      expect(ipcRenderer.invoke).toHaveBeenLastCalledWith(channels.invoke, {
        operation: 'approveExtension',
        id: 'run-1',
        approvalId: 'mcp-2',
        decision,
      })
    }
  })
  it('sends permanent revocation separately from stopping a conversation', async () => {
    await bridge.revokeControlPermission('plugin_chrome_use_chrome')
    expect(ipcRenderer.invoke).toHaveBeenCalledExactlyOnceWith(channels.invoke, {
      operation: 'revokeControlPermission',
      serverId: 'plugin_chrome_use_chrome',
    })
  })
})
