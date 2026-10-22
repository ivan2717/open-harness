export type McpTransport =
  | { type: 'stdio'; command: string; args: string[]; env_vars: string[] }
  | { type: 'http'; url: string; bearer_token_env?: string | null }

export interface McpServer {
  id: string
  name?: string | null
  enabled: boolean
  transport: McpTransport
}

export interface ExtensionConfig {
  revision?: number
  mcp_servers: McpServer[]
  disabled_skill_ids: string[]
}

export interface SkillInfo {
  id: string
  name: string
  description: string
  path: string
  scope: 'project' | 'user' | 'additional'
  enabled: boolean
}

export interface ExtensionSettings {
  config: ExtensionConfig
  skills_directory: string
  skills: SkillInfo[]
  diagnostics: { path: string; message: string }[]
}

export interface ExtensionApproval {
  id: string
  server_id: string
  server_name: string
  operation: string
  arguments: unknown
}

export interface RunExtensions {
  skills: SkillInfo[]
  diagnostics: { path: string; message: string }[]
  approvals: ExtensionApproval[]
}

export interface McpCatalog {
  tools: { name: string; description?: string | null; input_schema: unknown }[]
  [key: string]: unknown
}
