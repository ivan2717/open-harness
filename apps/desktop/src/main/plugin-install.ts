import { isAbsolute } from 'node:path'
import type { OpenDialogOptions, OpenDialogReturnValue } from 'electron'
import type { ApiRequest } from './protocol'
import { pluginIdentifier } from './extension-protocol'

/** Import paths only come from a native picker, never from renderer arguments. */
export async function installPluginFromDialog(
  expectedId: unknown,
  sourceKind: unknown,
  actions: {
    choose: (options: OpenDialogOptions) => Promise<OpenDialogReturnValue>
    request: (request: ApiRequest) => Promise<unknown>
  },
): Promise<unknown> {
  const expected_id = expectedId === undefined ? undefined : pluginIdentifier(expectedId)
  if (sourceKind !== 'directory' && sourceKind !== 'zip') throw new Error('无效的插件来源')
  const result = await actions.choose({
    title: expected_id ? '选择插件的新版本' : '安装插件',
    buttonLabel: expected_id ? '更新插件' : '安装插件',
    properties: [sourceKind === 'directory' ? 'openDirectory' : 'openFile'],
    ...(sourceKind === 'zip' ? { filters: [{ name: '插件压缩包', extensions: ['zip'] }] } : {}),
  })
  if (result.canceled || !result.filePaths.length) return null
  const source_path = result.filePaths[0]
  if (
    result.filePaths.length !== 1 ||
    !isAbsolute(source_path) ||
    source_path.length > 4096 ||
    source_path.includes('\0')
  )
    throw new Error('请选择一个插件文件夹或 ZIP 文件')
  return actions.request({
    path: '/api/plugins/install',
    method: 'POST',
    body: { source_path, ...(expected_id === undefined ? {} : { expected_id }) },
  })
}
