import type { ExtensionConfig, McpServer } from '../shared/extensions'

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('无效的扩展配置')
  return value as Record<string, unknown>
}
function text(value: unknown, limit: number): string {
  if (typeof value !== 'string' || value.length > limit || value.includes('\0'))
    throw new Error('无效的扩展配置字段')
  return value
}
function strings(value: unknown, count: number, limit: number): string[] {
  if (!Array.isArray(value) || value.length > count) throw new Error('扩展配置列表过大')
  return value.map((entry) => text(entry, limit))
}
function envName(value: unknown): string {
  const name = text(value, 128)
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error('请填写环境变量名称，不是密钥内容')
  return name
}

export function extensionConfiguration(value: unknown): ExtensionConfig {
  const input = object(value)
  const revision = input.revision ?? 0
  if (typeof revision !== 'number' || !Number.isSafeInteger(revision) || revision < 0)
    throw new Error('无效的扩展配置版本')
  if (!Array.isArray(input.mcp_servers) || input.mcp_servers.length > 32)
    throw new Error('最多配置 32 个 MCP 服务')
  const ids = new Set<string>()
  const mcp_servers: McpServer[] = input.mcp_servers.map((value) => {
    const server = object(value)
    const id = text(server.id, 64)
    if (!/^[a-zA-Z0-9_-]+$/.test(id) || ids.has(id)) throw new Error('MCP 服务标识必须唯一')
    ids.add(id)
    if (typeof server.enabled !== 'boolean') throw new Error('无效的 MCP 启用状态')
    const transport = object(server.transport)
    const base = {
      id,
      enabled: server.enabled,
      name: server.name == null ? null : text(server.name, 200),
    }
    if (transport.type === 'stdio') {
      const command = text(transport.command, 4096).trim()
      if (!command) throw new Error('请填写 MCP 启动程序')
      return {
        ...base,
        transport: {
          type: 'stdio',
          command,
          args: strings(transport.args, 128, 4096),
          env_vars: strings(transport.env_vars, 64, 128).map(envName),
        },
      }
    }
    if (transport.type === 'http') {
      const url = text(transport.url, 4096)
      const parsed = new URL(url)
      if (
        parsed.username ||
        parsed.password ||
        parsed.search ||
        parsed.hash ||
        (parsed.protocol !== 'https:' &&
          !(
            parsed.protocol === 'http:' &&
            ['localhost', '127.0.0.1', '[::1]'].includes(parsed.hostname)
          ))
      )
        throw new Error('MCP 地址须为 HTTPS 或本机 HTTP，不可包含账户、密码或查询参数')
      return {
        ...base,
        transport: {
          type: 'http',
          url,
          bearer_token_env:
            transport.bearer_token_env == null || transport.bearer_token_env === ''
              ? null
              : envName(transport.bearer_token_env),
        },
      }
    }
    throw new Error('不支持的 MCP 传输方式')
  })
  return { revision, mcp_servers, disabled_skill_ids: strings(input.disabled_skill_ids, 256, 128) }
}
