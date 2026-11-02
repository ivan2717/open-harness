import type { ChatMessage } from '../../shared/contracts'

export type ToolActivityKind =
  | 'command'
  | 'read'
  | 'write'
  | 'search'
  | 'skill'
  | 'browser'
  | 'computer'
  | 'agent'
  | 'integration'
  | 'other'

export interface ToolActivity {
  kind: ToolActivityKind
  /** An operation, not a claim that the operation succeeded. */
  label: string
  detail?: string
  /** The actual tool name, including the tool inside an MCP call. */
  operation: string
}

type Arguments = Record<string, unknown>

function object(value: unknown): Arguments {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Arguments)
    : {}
}

function parse(value: string | undefined): Arguments {
  if (!value) return {}
  try {
    return object(JSON.parse(value))
  } catch {
    return {}
  }
}

function compact(value: unknown, limit = 120): string | undefined {
  if (typeof value !== 'string') return undefined
  const text = value
    .replace(/[\u0000-\u001f\u007f]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
  if (!text) return undefined
  return text.length > limit ? `${text.slice(0, limit - 1)}…` : text
}

function urlLabel(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  try {
    const url = new URL(value)
    if (!['https:', 'http:'].includes(url.protocol)) return undefined
    // Credentials, query strings and fragments do not belong in activity titles.
    return compact(
      `${url.hostname}${url.port ? `:${url.port}` : ''}${url.pathname === '/' ? '' : url.pathname}`,
    )
  } catch {
    return undefined
  }
}

function tabLabel(args: Arguments): string | undefined {
  const value = args.tab_id
  return typeof value === 'string' || (typeof value === 'number' && Number.isFinite(value))
    ? compact(`标签页 ${value}`)
    : undefined
}

function browserActivity(
  operation: string,
  args: Arguments,
): Pick<ToolActivity, 'label' | 'detail'> {
  const action = operation.replace(/^(?:chrome|browser)_/, '')
  const detail = urlLabel(args.url) ?? urlLabel(args.origin) ?? tabLabel(args)
  if (action === 'action') {
    const labels: Record<string, string> = {
      click: '点击网页',
      fill: '填写网页',
      select: '选择网页选项',
      press: '按下网页按键',
      scroll: '滚动网页',
      back: '返回上一页',
      forward: '前往下一页',
    }
    return { label: compact(labels[String(args.action)]) ?? '操作网页', detail }
  }
  const labels: Record<string, string> = {
    status: '检查浏览器连接',
    tabs: '查看浏览器标签页',
    open: '打开网页',
    select_tab: '选择浏览器标签页',
    snapshot: '读取网页',
    screenshot: '查看网页截图',
    release: args.close_owned === true ? '关闭任务标签页' : '停止控制标签页',
    close: '关闭独立浏览器',
  }
  return { label: compact(labels[action]) ?? '使用浏览器', detail }
}

function computerActivity(
  operation: string,
  args: Arguments,
): Pick<ToolActivity, 'label' | 'detail'> {
  const action = operation.replace(/^computer_/, '')
  const labels: Record<string, string> = {
    status: '检查电脑操作权限',
    apps: '查看运行中的应用',
    select_app: '选择应用窗口',
    windows: '查看应用窗口',
    observe: args.screenshot === true ? '查看窗口截图' : '读取应用界面',
    click: '点击应用界面',
    type: '输入文字',
    key: '按下快捷键',
    scroll: '滚动应用界面',
    drag: '拖动界面元素',
    set_value: '填写应用内容',
    action: '操作界面控件',
    release: '停止控制应用',
  }
  // Do not surface typed text, field values or opaque snapshot references.
  const detail =
    action === 'select_app'
      ? compact(args.bundle_id)
      : action === 'key'
        ? compact(args.key)
        : undefined
  return { label: compact(labels[action]) ?? '操作电脑', detail }
}

function patchLabel(args: Arguments): string | undefined {
  if (typeof args.patch !== 'string') return undefined
  const paths = [...args.patch.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map(
    (match) => match[1].trim(),
  )
  return paths.length === 1
    ? compact(paths[0])
    : paths.length > 1
      ? `${paths.length} 个文件`
      : undefined
}

/** Project only recorded arguments; tool output must never be mistaken for a request. */
export function toolActivity(message: ChatMessage): ToolActivity {
  const operation = message.tool_name || '工具'
  const argumentsText = message.tool_arguments
  const args = parse(argumentsText ?? (message.status === 'pending' ? message.text : undefined))
  const describe = (kind: ToolActivityKind, label: string, detail?: string): ToolActivity => ({
    kind,
    label,
    detail,
    operation,
  })
  if (operation.startsWith('协作 · '))
    return describe('agent', '收到协作消息', compact(operation.slice('协作 · '.length)))
  if (operation === 'call_mcp_tool') {
    const name = compact(args.name, 100)
    const server = compact(args.server_id, 100)
    const nested = object(args.arguments)
    if (name && /^(?:chrome|browser)_/.test(name))
      return { kind: 'browser', ...browserActivity(name, nested), operation: name }
    if (name?.startsWith('computer_'))
      return { kind: 'computer', ...computerActivity(name, nested), operation: name }
    return {
      kind: 'integration',
      label: name ?? '调用外部工具',
      detail: server,
      operation: name ?? operation,
    }
  }
  // Native tool registrations may also expose these operations directly.
  if (/^(?:chrome|browser)_/.test(operation))
    return { kind: 'browser', ...browserActivity(operation, args), operation }
  if (operation.startsWith('computer_'))
    return { kind: 'computer', ...computerActivity(operation, args), operation }
  switch (operation) {
    case 'run_command':
      return describe('command', '运行命令', compact(args.command, 180))
    case 'manage_command':
      return describe(
        'command',
        args.action === 'stop'
          ? '停止命令'
          : args.action === 'status'
            ? '检查命令状态'
            : '读取命令输出',
      )
    case 'list_files':
      return describe('read', '查看文件', compact(args.path))
    case 'read_file':
      return describe('read', '读取文件', compact(args.path))
    case 'search_files':
      return describe('search', '搜索文件', compact(args.query) ?? compact(args.path))
    case 'write_file':
      return describe('write', '写入文件', compact(args.path))
    case 'edit_file':
      return describe('write', '编辑文件', compact(args.path))
    case 'apply_patch':
      return describe('write', '修改文件', patchLabel(args))
    case 'load_skill': {
      const output = message.status === 'complete' && !message.is_error ? parse(message.text) : {}
      return describe(
        'skill',
        '读取技能',
        compact(args.id) ?? compact(output.name) ?? compact(output.id),
      )
    }
    case 'list_skills':
      return describe('skill', '查找技能', compact(args.query))
    case 'load_workspace_dependencies':
      return describe('read', '检查脚本运行环境')
    case 'spawn_agent':
      return describe('agent', '启动协作任务', compact(args.task_name))
    case 'agent_control': {
      const labels: Record<string, string> = {
        list: '查看协作任务',
        send: '发送协作消息',
        wait: '等待协作任务',
        read: '读取协作记录',
        result: '查看协作结果',
        followup: '继续协作任务',
        resume: '继续协作任务',
        interrupt: '停止协作任务',
        cancel: '停止协作任务',
        models: '查看可用模型',
      }
      return describe(
        'agent',
        compact(labels[String(args.action)]) ?? '协调任务',
        compact(args.target) ?? compact(args.agent_id),
      )
    }
    case 'list_mcp_servers':
      return describe('integration', '查找扩展服务')
    case 'list_mcp_tools':
      return describe('integration', '查找可用工具', compact(args.server_id))
    case 'list_mcp_resources':
      return describe('integration', '查找扩展资源', compact(args.server_id))
    case 'read_mcp_resource':
      return describe('read', '读取扩展资源', compact(args.uri))
    case 'list_mcp_prompts':
      return describe('integration', '查找扩展提示', compact(args.server_id))
    case 'get_mcp_prompt':
      return describe('integration', '读取扩展提示', compact(args.name))
    case 'read_run_history':
      return describe('read', '回查对话记录')
    case 'search_run_history':
      return describe('search', '搜索对话记录', compact(args.query))
    case 'session_memory':
      return describe(
        args.action === 'search' ? 'search' : 'read',
        '查看会话记忆',
        compact(args.query) ?? compact(args.page_id),
      )
    default:
      return describe('other', compact(operation) ?? '使用工具')
  }
}
