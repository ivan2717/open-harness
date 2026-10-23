import { describe, expect, it, vi } from 'vitest'
import { installPluginFromDialog } from './plugin-install'
import { apiRequest } from './protocol'

describe('plugin installation bridge', () => {
  it('only imports the path returned by the native picker', async () => {
    const choose = vi.fn().mockResolvedValue({ canceled: false, filePaths: ['/tmp/review.zip'] })
    const request = vi.fn().mockResolvedValue({ plugins: [] })
    await installPluginFromDialog('review', 'zip', { choose, request })
    expect(choose.mock.calls[0][0]).toMatchObject({
      properties: ['openFile'],
      filters: [{ name: '插件压缩包', extensions: ['zip'] }],
    })
    expect(request).toHaveBeenCalledWith({
      path: '/api/plugins/install',
      method: 'POST',
      body: { source_path: '/tmp/review.zip', expected_id: 'review' },
    })
    expect(() =>
      apiRequest({ operation: 'installPlugin', source_path: '/private/secret' }),
    ).toThrow()
  })

  it('cancels without touching the runtime and rejects invalid requests before opening a picker', async () => {
    const choose = vi.fn().mockResolvedValue({ canceled: true, filePaths: [] })
    const request = vi.fn()
    expect(await installPluginFromDialog(undefined, 'directory', { choose, request })).toBeNull()
    expect(choose.mock.calls[0][0].properties).toEqual(['openDirectory'])
    expect(request).not.toHaveBeenCalled()
    choose.mockClear()
    await expect(
      installPluginFromDialog('../outside', 'directory', { choose, request }),
    ).rejects.toThrow()
    await expect(installPluginFromDialog('review', 'http', { choose, request })).rejects.toThrow()
    expect(choose).not.toHaveBeenCalled()
    expect(request).not.toHaveBeenCalled()
  })

  it('validates plugin enable and uninstall operations independently of run identifiers', () => {
    expect(apiRequest({ operation: 'setPluginEnabled', id: 'review', enabled: false })).toEqual({
      path: '/api/plugins/review/enabled',
      method: 'POST',
      body: { enabled: false },
    })
    expect(apiRequest({ operation: 'uninstallPlugin', id: 'review' })).toEqual({
      path: '/api/plugins/review',
      method: 'DELETE',
    })
    expect(() =>
      apiRequest({ operation: 'setPluginEnabled', id: 'review', enabled: 'true' }),
    ).toThrow()
    expect(() => apiRequest({ operation: 'uninstallPlugin', id: 'x'.repeat(65) })).toThrow()
    expect(() => apiRequest({ operation: 'uninstallPlugin', id: '../review' })).toThrow()
    expect(() => apiRequest({ operation: 'uninstallPlugin', id: 'org..review' })).toThrow()
    expect(apiRequest({ operation: 'uninstallPlugin', id: 'org.review' }).path).toBe(
      '/api/plugins/org.review',
    )
  })
})
