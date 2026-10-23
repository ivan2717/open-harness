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
  /** Enabled by default when upgrading configurations without this field. */
  llm_wiki_enabled?: boolean
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
  plugin_id?: string | null
}

export interface SkillCatalog {
  skills: SkillInfo[]
  diagnostics: { path: string; message: string }[]
}

export type PluginSourceKind = 'directory' | 'zip'

export interface PluginInfo {
  id: string
  name: string
  version: string
  description: string
  enabled: boolean
  source_path: string
  content_hash: string
  skill_count: number
  mcp_servers: McpServer[]
  warnings: string[]
}

export interface ExtensionSettings {
  config: ExtensionConfig
  builtin_plugins: BuiltinPluginInfo[]
  plugins: PluginInfo[]
  skills_directory: string
  skills: SkillInfo[]
  diagnostics: { path: string; message: string }[]
}

export interface BuiltinPluginInfo {
  id: string
  name: string
  description: string
  enabled: boolean
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
