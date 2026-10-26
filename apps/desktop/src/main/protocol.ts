import type {
  ConfigureModelInput,
  CreateRunInput,
  ResolvedModelConfiguration,
  TextFileReference,
  UpdateRunModelInput,
} from '../shared/contracts'
import { MODEL_TOKEN_LIMIT, resolveModelLimits } from '../shared/model-limits'
import { validReasoningId, validateReasoningCapability } from '../shared/model-reasoning'
import type { ExtensionConfig } from '../shared/extensions'
import { extensionConfiguration, pluginIdentifier } from './extension-protocol'
import {
  MAX_REFERENCE_FILES,
  MAX_FILE_REFERENCE_BYTES,
  MAX_REFERENCE_TOTAL_BYTES,
} from '../shared/file-references'

export const channels = {
  openSkillsDirectory: 'openharness:skills:open-directory',
  installPlugin: 'openharness:plugins:install',
  bundledPlugins: 'openharness:plugins:bundled',
  installBundledPlugin: 'openharness:plugins:install-bundled',
  prepareBrowser: 'openharness:browser:prepare',
  connectChrome: 'openharness:chrome:connect',
  chromeConnectionStatus: 'openharness:chrome:status',
  computerSettings: 'openharness:computer:settings',
  resolveResource: 'openharness:resource:resolve',
  resourceAction: 'openharness:resource:action',
  openExternal: 'openharness:resource:external',
  copyText: 'openharness:clipboard:copy-text',
  invoke: 'openharness:invoke',
  chooseWorkspace: 'openharness:workspace:choose',
  openWorkspace: 'openharness:workspace:open',
  chooseFiles: 'openharness:files:choose',
  importFiles: 'openharness:files:import',
  pasteFiles: 'openharness:files:paste',
  watchStart: 'openharness:watch:start',
  watchStop: 'openharness:watch:stop',
  watchEvent: 'openharness:watch:event',
} as const

export type BridgeRequest =
  | { operation: 'extensionSettings' }
  | { operation: 'skillCatalog'; workspacePath?: string }
  | { operation: 'setPluginEnabled'; id: string; enabled: boolean }
  | { operation: 'uninstallPlugin'; id: string }
  | { operation: 'saveExtensions'; config: ExtensionConfig }
  | { operation: 'testMcp' | 'runExtensions' | 'refreshRunSkills' | 'runMemory'; id: string }
  | {
      operation: 'approveExtension'
      id: string
      approvalId: string
      allow: boolean
      session?: boolean
    }
  | { operation: 'revokeControl'; id: string }
  | { operation: 'config' | 'listRuns' }
  | {
      operation: 'getRun' | 'getWorkspace' | 'cancelRun' | 'compactRun' | 'cancelCompaction'
      id: string
    }
  | { operation: 'approveCommand'; id: string; approvalId: string; allow: boolean }
  | { operation: 'revertFile'; id: string; path: string }
  | { operation: 'resumeRun'; id: string; message?: string; files?: TextFileReference[] }
  | { operation: 'configureModel'; input: ConfigureModelInput }
  | { operation: 'refreshModelContext'; id: string }
  | { operation: 'createRun'; input: CreateRunInput }
  | { operation: 'updateRunModel'; id: string; input: UpdateRunModelInput }
  | { operation: 'submitInput'; id: string; message: string; files?: TextFileReference[] }

export interface ApiRequest {
  path: string
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE'
  /** Main-process access check only; never forwarded in the HTTP body. */
  workspacePath?: string
  body?:
    | ExtensionConfig
    | CreateRunInput
    | ConfigureModelInput
    | UpdateRunModelInput
    | { message?: string; files?: TextFileReference[] }
    | { path: string }
    | { allow: boolean }
    | { enabled: boolean }
    | { source_path: string; expected_id?: string }
    | { replacement_model_id: string }
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Invalid request')
  return value as Record<string, unknown>
}

export function identifier(value: unknown): string {
  if (typeof value !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(value))
    throw new Error('Invalid run identifier')
  return value
}

function boundedText(value: unknown, maximum: number, field: string): string {
  if (typeof value !== 'string' || !value.trim() || value.length > maximum)
    throw new Error(`Invalid ${field}`)
  return value
}

/** Paths in these snapshots are descriptive; they never grant filesystem access. */
export function fileReferences(value: unknown): TextFileReference[] | undefined {
  if (value === undefined) return undefined
  if (!Array.isArray(value) || value.length > MAX_REFERENCE_FILES)
    throw new Error('每条消息最多引用 8 个文件。')
  let total = 0
  return value.map((item: unknown) => {
    const file = object(item)
    if (
      typeof file.name !== 'string' ||
      !file.name.trim() ||
      [...file.name].length > 255 ||
      /[\/\\\u0000-\u001f\u007f-\u009f]/.test(file.name)
    )
      throw new Error('文件名称无效。')
    if (
      file.path !== undefined &&
      (typeof file.path !== 'string' ||
        !file.path ||
        [...file.path].length > 4096 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(file.path))
    )
      throw new Error('文件路径无效。')
    if (
      typeof file.text !== 'string' ||
      /[\u0000-\u0008\u000b-\u000c\u000e-\u001f\u007f-\u009f]/.test(file.text)
    )
      throw new Error('仅支持 UTF-8 文本文件；暂不支持图片、PDF 或二进制文件。')
    const bytes = new TextEncoder().encode(file.text).byteLength
    if (bytes > MAX_FILE_REFERENCE_BYTES) throw new Error('单个引用文件不能超过 256 KiB。')
    total += bytes
    if (total > MAX_REFERENCE_TOTAL_BYTES) throw new Error('引用文件合计不能超过 512 KiB。')
    return { name: file.name, ...(file.path ? { path: file.path as string } : {}), text: file.text }
  })
}

function userText(value: unknown, files?: TextFileReference[]): string {
  if (typeof value !== 'string' || [...value].length > 4000 || (!value.trim() && !files?.length))
    throw new Error('请输入消息或添加文件引用，正文最多 4,000 个字符。')
  return value
}

function reasoningEffort(value: unknown): string | undefined {
  if (value === undefined || value === null) return undefined
  // The backend validates the selected model's advertised choices. This
  // boundary only admits short names, never arbitrary provider payloads.
  if (!validReasoningId(value)) throw new Error('无效的思考强度')
  return value
}

function absolutePath(path: string): boolean {
  return path.startsWith('/') || path.startsWith('\\\\') || /^[A-Za-z]:[\\/]/.test(path)
}

export function modelConfiguration(value: unknown): ConfigureModelInput {
  const input = object(value)
  const protocol = input.protocol
  if (
    protocol !== 'chat_completions' &&
    protocol !== 'responses' &&
    protocol !== 'anthropic_messages'
  )
    throw new Error('Invalid model protocol')
  const base_url = boundedText(input.base_url, 2048, 'base URL').trim()
  let url: URL
  try {
    url = new URL(base_url)
  } catch {
    throw new Error('Invalid model base URL')
  }
  const local = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
  if (
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && local)) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    throw new Error('Use an HTTPS model base URL, or HTTP on localhost')
  const integer = (value: unknown, min: number, max: number, name: string): number => {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < min || value > max)
      throw new Error(`Invalid ${name}`)
    return value
  }
  const context_window =
    input.context_window === undefined
      ? undefined
      : integer(input.context_window, 1024, MODEL_TOKEN_LIMIT, 'context window')
  const max_output_tokens =
    input.max_output_tokens === undefined
      ? undefined
      : integer(input.max_output_tokens, 64, MODEL_TOKEN_LIMIT, 'output limit')
  const reasoning_mode = input.reasoning_mode
  const reasoning_effort = input.reasoning_effort
  if (
    !['off', 'standard', 'deepseek'].includes(String(reasoning_mode)) ||
    !validReasoningId(reasoning_effort) ||
    (reasoning_mode === 'deepseek' && protocol !== 'chat_completions')
  )
    throw new Error('Invalid reasoning configuration')
  const reasoning_capability =
    input.reasoning_capability === undefined
      ? undefined
      : validateReasoningCapability(input.reasoning_capability, protocol)
  const reasoning_default = reasoningEffort(input.reasoning_default)
  if (reasoning_capability && reasoning_mode !== 'off')
    throw new Error('请使用模型思考能力选择默认强度，不能同时使用旧思考模式')
  if (reasoning_default && reasoning_capability) {
    const selected = reasoning_capability.options.find((option) => option.id === reasoning_default)
    if (
      !selected ||
      (max_output_tokens !== undefined &&
        selected.budget_tokens !== undefined &&
        selected.budget_tokens >= max_output_tokens)
    )
      throw new Error('所选思考设置不受支持或超过输出上限')
  }
  const configuration: ConfigureModelInput = {
    protocol,
    base_url,
    context_window,
    max_output_tokens,
    model: boundedText(input.model, 256, 'model').trim(),
    api_key: boundedText(input.api_key, 4096, 'API key').trim(),
    reasoning_mode: reasoning_mode as ConfigureModelInput['reasoning_mode'],
    reasoning_effort: reasoning_effort as ConfigureModelInput['reasoning_effort'],
    ...(reasoning_capability ? { reasoning_capability } : {}),
    ...(input.reasoning_default !== undefined
      ? { reasoning_default: reasoning_default ?? null }
      : {}),
    ...(input.supports_images === undefined
      ? {}
      : { supports_images: input.supports_images === true }),
  }
  if (input.supports_images !== undefined && typeof input.supports_images !== 'boolean')
    throw new Error('Invalid image capability')
  if (context_window !== undefined && max_output_tokens !== undefined)
    resolveModelLimits(configuration)
  return configuration
}

/** Saved configurations and the Rust API always receive final, validated numbers. */
export function resolvedModelConfiguration(value: unknown): ResolvedModelConfiguration {
  const configuration = modelConfiguration(value)
  const { context_window, max_output_tokens } = configuration
  if (context_window === undefined || max_output_tokens === undefined)
    throw new Error('Model capacities must be resolved before saving')
  if (configuration.reasoning_default && !configuration.reasoning_capability)
    throw new Error('选择思考强度前需要声明模型能力')
  return { ...configuration, context_window, max_output_tokens }
}

export function apiRequest(value: unknown): ApiRequest {
  const request = object(value)
  switch (request.operation) {
    case 'setPluginEnabled':
      if (typeof request.enabled !== 'boolean') throw new Error('无效的插件启用状态')
      return {
        path: `/api/plugins/${pluginIdentifier(request.id)}/enabled`,
        method: 'POST',
        body: { enabled: request.enabled },
      }
    case 'uninstallPlugin':
      return { path: `/api/plugins/${pluginIdentifier(request.id)}`, method: 'DELETE' }
    case 'extensionSettings':
      return { path: '/api/extensions', method: 'GET' }
    case 'skillCatalog': {
      if (request.workspacePath === undefined)
        return { path: '/api/extensions/skills', method: 'GET' }
      const path = boundedText(request.workspacePath, 4096, 'project path')
      if (!absolutePath(path) || /[\u0000-\u001f\u007f]/.test(path))
        throw new Error('Invalid project path')
      return {
        path: `/api/extensions/skills?workspace_path=${encodeURIComponent(path)}`,
        method: 'GET',
        workspacePath: path,
      }
    }
    case 'saveExtensions':
      return {
        path: '/api/extensions',
        method: 'POST',
        body: extensionConfiguration(request.config),
      }
    case 'testMcp':
      return { path: `/api/extensions/mcp/${identifier(request.id)}/test`, method: 'POST' }
    case 'runExtensions':
      return { path: `/api/runs/${identifier(request.id)}/extensions`, method: 'GET' }
    case 'runMemory':
      return { path: `/api/runs/${identifier(request.id)}/memory`, method: 'GET' }
    case 'refreshRunSkills':
      return { path: `/api/runs/${identifier(request.id)}/extensions/refresh`, method: 'POST' }
    case 'approveExtension':
      if (typeof request.allow !== 'boolean') throw new Error('Invalid approval decision')
      if (request.session !== undefined && typeof request.session !== 'boolean')
        throw new Error('Invalid approval scope')
      return {
        path: `/api/runs/${identifier(request.id)}/extensions/approvals/${identifier(request.approvalId)}`,
        method: 'POST',
        body: {
          allow: request.allow,
          ...(request.session === undefined ? {} : { session: request.session }),
        },
      }
    case 'revokeControl':
      return { path: `/api/runs/${identifier(request.id)}/extensions/control`, method: 'DELETE' }
    case 'config':
      return { path: '/api/config', method: 'GET' }
    case 'listRuns':
      return { path: '/api/runs', method: 'GET' }
    case 'getRun':
      return { path: `/api/runs/${identifier(request.id)}`, method: 'GET' }
    case 'getWorkspace':
      return { path: `/api/runs/${identifier(request.id)}/workspace`, method: 'GET' }
    case 'approveCommand':
      if (typeof request.allow !== 'boolean') throw new Error('Invalid approval decision')
      return {
        path: `/api/runs/${identifier(request.id)}/approvals/${identifier(request.approvalId)}`,
        method: 'POST',
        body: { allow: request.allow },
      }
    case 'revertFile': {
      const path = boundedText(request.path, 4096, 'file path')
      if (absolutePath(path) || path.includes('\0') || path.split(/[\\/]/).includes('..'))
        throw new Error('File path must be relative to the project')
      return { path: `/api/runs/${identifier(request.id)}/revert`, method: 'POST', body: { path } }
    }
    case 'resumeRun': {
      const files = fileReferences(request.files)
      return {
        path: `/api/runs/${identifier(request.id)}/resume`,
        method: 'POST',
        body:
          request.message === undefined && !files?.length
            ? {}
            : {
                message: userText(request.message ?? '', files),
                ...(files?.length ? { files } : {}),
              },
      }
    }
    case 'cancelRun':
      return { path: `/api/runs/${identifier(request.id)}/cancel`, method: 'POST' }
    case 'compactRun':
      return { path: `/api/runs/${identifier(request.id)}/compact`, method: 'POST' }
    case 'cancelCompaction':
      return { path: `/api/runs/${identifier(request.id)}/compact/cancel`, method: 'POST' }
    case 'configureModel':
      return { path: '/api/models', method: 'POST', body: modelConfiguration(request.input) }
    case 'refreshModelContext':
      return { path: `/api/models/${identifier(request.id)}/context/refresh`, method: 'POST' }
    case 'updateRunModel': {
      const input = object(request.input)
      return {
        path: `/api/runs/${identifier(request.id)}/model`,
        method: 'PATCH',
        body: {
          model_id: identifier(input.model_id),
          reasoning_effort: reasoningEffort(input.reasoning_effort) ?? null,
        },
      }
    }
    case 'createRun': {
      const input = object(request.input)
      const files = fileReferences(input.files)
      const reasoning_effort = reasoningEffort(input.reasoning_effort)
      let workspace: Pick<CreateRunInput, 'workspace_path' | 'permission_mode'> = {}
      if (input.workspace_path !== undefined) {
        const path = boundedText(input.workspace_path, 4096, 'project path')
        if (!absolutePath(path) || path.includes('\0')) throw new Error('Invalid project path')
        const mode = input.permission_mode ?? 'read_only'
        if (mode !== 'read_only' && mode !== 'workspace_write')
          throw new Error('Invalid permission mode')
        workspace = { workspace_path: path, permission_mode: mode }
      } else if (input.permission_mode !== undefined) {
        throw new Error('Choose a project before setting permissions')
      }
      return {
        path: '/api/runs',
        method: 'POST',
        body: {
          prompt: userText(input.prompt, files),
          model_id: boundedText(input.model_id, 256, 'model'),
          ...(reasoning_effort ? { reasoning_effort } : {}),
          ...(files?.length ? { files } : {}),
          ...workspace,
        },
      }
    }
    case 'submitInput': {
      const files = fileReferences(request.files)
      return {
        path: `/api/runs/${identifier(request.id)}/input`,
        method: 'POST',
        body: { message: userText(request.message, files), ...(files?.length ? { files } : {}) },
      }
    }
    default:
      throw new Error('Unsupported desktop operation')
  }
}

export function watchRequest(value: unknown): { id: string; token: string } {
  const request = object(value)
  return { id: identifier(request.id), token: identifier(request.token) }
}
