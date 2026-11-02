import { describe, expect, it } from 'vitest'
import type { ChatMessage } from '../../shared/contracts'
import { toolActivity } from './tool-activity'

function tool(name: string, args?: unknown, overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: 'tool-test',
    role: 'tool',
    tool_name: name,
    text: '{"ok":true}',
    status: 'complete',
    ...(args === undefined ? {} : { tool_arguments: JSON.stringify(args) }),
    ...overrides,
  }
}

function mcp(name: string, args: unknown = {}, server = 'plugin:chrome-use:chrome') {
  return tool('call_mcp_tool', { server_id: server, name, arguments: args })
}

describe('tool activity descriptions', () => {
  it('keeps command descriptions after their output replaces the request body', () => {
    expect(
      toolActivity(tool('run_command', { command: 'git status\n--short --branch' })),
    ).toMatchObject({
      kind: 'command',
      label: '运行命令',
      detail: 'git status --short --branch',
      operation: 'run_command',
    })
    expect(toolActivity(tool('run_command', { command: 'x'.repeat(300) })).detail).toHaveLength(180)
    expect(
      toolActivity(tool('manage_command', { action: 'stop', command_id: 'private-id' })),
    ).toMatchObject({ label: '停止命令', detail: undefined })
  })

  it('describes file operations, edits and skill loading from real schemas', () => {
    expect(
      toolActivity(tool('read_file', { path: 'docs/说明.pdf', cursor: { unit: 2, offset: 0 } })),
    ).toMatchObject({ label: '读取文件', detail: 'docs/说明.pdf' })
    expect(toolActivity(tool('search_files', { path: '.', query: 'preview' }))).toMatchObject({
      kind: 'search',
      detail: 'preview',
    })
    expect(
      toolActivity(
        tool('apply_patch', {
          patch: '*** Begin Patch\n*** Update File: src/main.ts\n@@\n-a\n+b\n*** End Patch',
        }),
      ),
    ).toMatchObject({ kind: 'write', detail: 'src/main.ts' })
    expect(
      toolActivity(
        tool('apply_patch', { patch: '*** Add File: one.ts\n+x\n*** Delete File: two.ts' }),
      ).detail,
    ).toBe('2 个文件')
    expect(toolActivity(tool('load_skill', { id: 'documents:pdf' }))).toMatchObject({
      kind: 'skill',
      label: '读取技能',
      detail: 'documents:pdf',
    })
  })

  it('unwraps MCP browser operations without displaying wrapper names or private input', () => {
    expect(
      toolActivity(
        mcp('chrome_open', { url: 'https://user:secret@example.com/articles?q=token#private' }),
      ),
    ).toEqual({
      kind: 'browser',
      label: '打开网页',
      detail: 'example.com/articles',
      operation: 'chrome_open',
    })
    expect(toolActivity(mcp('chrome_snapshot', { tab_id: 123 }))).toMatchObject({
      label: '读取网页',
      detail: '标签页 123',
    })
    expect(
      toolActivity(
        mcp('chrome_action', {
          revision: 'opaque',
          action: 'fill',
          ref: 'e1',
          text: 'secret password',
        }),
      ),
    ).toMatchObject({ label: '填写网页', detail: undefined })
    expect(toolActivity(mcp('browser_action', { revision: 1, action: 'back' }))).toMatchObject({
      label: '返回上一页',
    })
    expect(toolActivity(mcp('chrome_release', { close_owned: true }))).toMatchObject({
      label: '关闭任务标签页',
    })
  })

  it('describes desktop operations without copying typed values or snapshot tokens into titles', () => {
    expect(
      toolActivity(mcp('computer_select_app', { bundle_id: 'com.tencent.xinWeChat', pid: 200 })),
    ).toMatchObject({ kind: 'computer', label: '选择应用窗口', detail: 'com.tencent.xinWeChat' })
    expect(toolActivity(mcp('computer_observe', { screenshot: true }))).toMatchObject({
      label: '查看窗口截图',
    })
    expect(
      toolActivity(mcp('computer_type', { snapshot_id: 'private snapshot', text: 'private text' })),
    ).toMatchObject({ label: '输入文字', detail: undefined })
    expect(
      toolActivity(mcp('computer_key', { key: 'meta+a', snapshot_id: 'private snapshot' })),
    ).toMatchObject({ label: '按下快捷键', detail: 'meta+a' })
  })

  it('keeps unknown integrations identifiable without guessing their semantics', () => {
    expect(toolActivity(mcp('list_issues', { query: 'test' }, 'github'))).toEqual({
      kind: 'integration',
      label: 'list_issues',
      detail: 'github',
      operation: 'list_issues',
    })
    expect(toolActivity(tool('custom_tool', { secret: 'hidden' }))).toMatchObject({
      kind: 'other',
      label: 'custom_tool',
      detail: undefined,
    })
  })

  it('uses task names and targets for collaboration without asserting task success', () => {
    expect(
      toolActivity(tool('spawn_agent', { task_name: 'review', task: 'Private instructions' })),
    ).toMatchObject({ kind: 'agent', label: '启动协作任务', detail: 'review' })
    expect(
      toolActivity(tool('agent_control', { action: 'followup', target: '/root/review' })),
    ).toMatchObject({ label: '继续协作任务', detail: '/root/review' })
    expect(
      toolActivity(
        tool('agent_control', { action: 'interrupt', target: '/root/review' }, { is_error: true }),
      ),
    ).toMatchObject({ label: '停止协作任务' })
    expect(
      toolActivity(tool('agent_control', { action: 'result', target: 'review' })),
    ).toMatchObject({ label: '查看协作结果' })
  })

  it('recognizes persisted collaborator messages without exposing their report in the label', () => {
    expect(
      toolActivity(tool('协作 · /root/review', undefined, { text: 'Detailed private report' })),
    ).toEqual({
      kind: 'agent',
      label: '收到协作消息',
      detail: '/root/review',
      operation: '协作 · /root/review',
    })
  })

  it('supports pending legacy records without treating completed output as request arguments', () => {
    expect(
      toolActivity(
        tool('read_file', undefined, { status: 'pending', text: '{"path":"README.md"}' }),
      ).detail,
    ).toBe('README.md')
    expect(
      toolActivity(tool('read_file', undefined, { text: '{"path":"not a request"}' })).detail,
    ).toBeUndefined()
    expect(
      toolActivity(
        tool('call_mcp_tool', undefined, {
          text: '{"name":"chrome_open","arguments":{"url":"https://wrong.example"}}',
        }),
      ).label,
    ).toBe('调用外部工具')
    expect(
      toolActivity(
        tool('load_skill', undefined, { text: '{"name":"Review","content":"Instructions"}' }),
      ).detail,
    ).toBe('Review')
  })

  it('tolerates malformed or unexpected argument shapes and never interpolates objects', () => {
    for (const args of [null, [], 12, 'unparsed', { name: {}, arguments: [] }]) {
      expect(toolActivity(tool('call_mcp_tool', args)).label).toBe('调用外部工具')
    }
    expect(
      toolActivity(tool('call_mcp_tool', undefined, { status: 'pending', text: 'invalid json' }))
        .kind,
    ).toBe('integration')
    expect(toolActivity(tool('run_command', { command: {} })).detail).toBeUndefined()
    expect(toolActivity(mcp('chrome_open', { url: 'javascript:alert(1)' })).detail).toBeUndefined()
    expect(toolActivity(tool('agent_control', { action: '__proto__' })).label).toBe('协调任务')
    expect(toolActivity(mcp('chrome_action', { action: 'constructor' })).label).toBe('操作网页')
  })
})
