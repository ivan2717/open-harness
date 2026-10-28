import type { ModelCapacity, ResolvedModelLimits } from './model-limits'
import type { ReasoningCapability } from './model-reasoning'
export type { ReasoningCapability, ReasoningOption } from './model-reasoning'
import type {
  ControlApprovalDecision,
  ExtensionConfig,
  ExtensionSettings,
  McpCatalog,
  RunExtensions,
  PluginSourceKind,
  SkillCatalog,
  BundledPlugin,
} from './extensions'
import type { ResolvedResource, ResourceAction, ResourceReference } from './message-resources'
import type { RunMemory } from './memory'

export type RunStatus =
  'running' | 'waiting_for_input' | 'completed' | 'failed' | 'cancelled' | 'interrupted' | 'paused'

export interface RunDelegation {
  parent_run_id: string
  root_run_id: string
  task_name: string
  write_paths?: string[] | null
  model_override?: UpdateRunModelInput | null
}

export interface ExecutionPolicy {
  max_steps?: number | null
  max_agents?: number
}

export type PermissionMode = 'read_only' | 'workspace_write'

export interface ModelInfo {
  id: string
  name: string
  description: string
  available: boolean
  context_window: number
  supports_images?: boolean
  reasoning?: {
    default_effort: string | null
    supported_efforts: string[]
    capability?: ReasoningCapability
  }
}

export interface AppConfig {
  version: string
  models: ModelInfo[]
  default_model: string
  storage_path: string
  model_config_error?: string
}

export interface ConfigureModelInput {
  protocol: 'chat_completions' | 'responses' | 'anthropic_messages'
  base_url: string
  api_key: string
  model: string
  /** Omit to resolve automatically when saving the model. */
  context_window?: number
  /** Per-request output budget, not the provider's physical maximum. */
  max_output_tokens?: number
  reasoning_mode: 'off' | 'standard' | 'deepseek'
  reasoning_effort: string
  /** Omitted for legacy profiles; resolved independently of the default selection. */
  reasoning_capability?: ReasoningCapability
  /** Omitted/null leaves the provider's default parameters unchanged. */
  reasoning_default?: string | null
  supports_images?: boolean
}

export type ResolvedModelConfiguration = Omit<
  ConfigureModelInput,
  'context_window' | 'max_output_tokens'
> & {
  context_window: number
  max_output_tokens: number
}

/** Editable settings without returning the saved credential to the renderer. */
export type ModelConfiguration = Omit<ResolvedModelConfiguration, 'api_key'> & {
  id: string
  has_api_key: boolean
}

/** An omitted or blank key keeps the credential for the same service origin. */
export type UpdateModelInput = Omit<ConfigureModelInput, 'api_key'> & { api_key?: string }

export interface ConfigureModelResult {
  model: ModelInfo
  persisted: boolean
  limits: ResolvedModelLimits
  capacity?: ModelCapacity
  warning?: string
}

export interface RunSummary {
  id: string
  /** Monotonic snapshot revision, including transient stream updates. */
  revision: number
  title: string
  prompt: string
  status: RunStatus
  /** Manual context work remains active even while the run is waiting for input. */
  compacting?: boolean
  delegation?: RunDelegation | null
  execution_policy?: ExecutionPolicy
  model_id: string
  /** Omitted/null inherits this model configuration's default. */
  reasoning_effort?: string | null
  workspace_path?: string | null
  /** Backend-owned directory for a conversation that has no selected project. */
  execution_directory?: string | null
  permission_mode?: PermissionMode
  created_at: string
  updated_at: string
}

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant' | 'tool'
  text: string
  files?: FileReferenceInfo[]
  images?: { sequence: number; mime_type: string }[]
  reasoning?: string
  tool_name?: string
  is_error?: boolean
  status: 'pending' | 'streaming' | 'complete'
}

export interface TimelineEvent {
  sequence: number
  step: number
  type: string
  label: string
  detail: string
  timestamp: string
}

export interface CompactionRecord {
  id: string
  before_tokens: number
  after_tokens: number
  summary: string
  created_at: string
}

export interface ContextSnapshot {
  window_tokens: number
  input_budget: number
  estimated_input_tokens: number
  trigger_tokens: number
  target_tokens: number
  reserved_output_tokens: number
  compaction_count: number
  measurement:
    | 'estimated'
    | 'provider'
    | 'provider_calibrated'
    | 'provider_native'
    // Read compatibility only; current requests no longer produce these two modes.
    | 'provider_counted'
    | 'tokenizer_estimated'
  accounting?: {
    raw_estimated_input_tokens: number
    estimated_input_tokens: number
    measured_input_tokens: number | null
    estimated_delta_tokens: number
    estimation_margin_tokens: number
    decision_input_tokens: number
    source:
      | 'estimated'
      | 'provider_calibrated'
      | 'provider_native'
      // Retired counting modes may still appear in saved snapshots.
      | 'provider_counted'
      | 'tokenizer_estimated'
  }
  /** Only usage samples committed with successful responses; not context size. */
  observed_usage?: {
    input_tokens: number
    output_tokens: number
    responses: number
  }
  checkpoints: CompactionRecord[]
}

export interface RunDetail extends RunSummary {
  messages: ChatMessage[]
  events: TimelineEvent[]
  context: ContextSnapshot | null
  error?: string
}

export interface CreateRunInput {
  prompt: string
  model_id: string
  reasoning_effort?: string
  files?: FileReference[]
  workspace_path?: string
  permission_mode?: PermissionMode
}

export interface UpdateRunModelInput {
  model_id: string
  /** Omitted/null resets the conversation override to the selected model's default. */
  reasoning_effort?: string | null
}

/** A user-selected UTF-8 file snapshot, not a grant to its parent directory. */
export interface TextFileReference {
  name: string
  path?: string
  text: string
  image?: never
  attachment?: never
}

/** A captured image, independent of the clipboard or original file. */
export interface ImageFileReference {
  name: string
  path?: string
  image: { mime_type: string; data_base64: string }
  text?: never
  attachment?: never
}

/** Original bytes retained locally; the model reads selected portions through file tools. */
export interface StoredFileReference {
  name: string
  path?: string
  attachment: { id: string; size: number }
  text?: never
  image?: never
}

export type FileReference = TextFileReference | ImageFileReference | StoredFileReference

export interface FileReferenceInfo {
  name: string
  path?: string
  size: number
  mime_type?: string
}

export interface FileImportResult {
  files: FileReference[]
  errors: string[]
}

export interface FileChange {
  path: string
  status: string
  before: string | null
  after: string | null
  revertible: boolean
}

export type CommandExecutionMode = 'sandboxed' | 'unrestricted' | 'unavailable'
export type CommandNetworkAccess = 'denied' | 'allowed'
export type CommandAccess = 'read_only' | 'workspace_write'
export type CommandApprovalScope = 'once' | 'session'

export interface CommandGrant {
  access: CommandAccess
  network_access: CommandNetworkAccess
}

export interface PendingApproval {
  id: string
  task_title?: string
  command: string
  cwd: string
  execution_mode?: CommandExecutionMode
  access?: CommandAccess
  network_access?: CommandNetworkAccess
}

export interface CommandRecord {
  id: string
  task_title?: string
  command: string
  status: string
  output: string
  exit_code: number | null
  access?: CommandAccess
  network_access?: CommandNetworkAccess
}

export interface WorkspaceReview {
  root: string
  permission_mode: PermissionMode
  busy: boolean
  sandbox_available: boolean
  execution_mode?: CommandExecutionMode
  review_incomplete?: boolean
  changes: FileChange[]
  approvals: PendingApproval[]
  commands: CommandRecord[]
  command_grants?: CommandGrant[]
}

export interface DesktopBridge {
  bundledPlugins(): Promise<BundledPlugin[]>
  installBundledPlugin(id: BundledPlugin['id']): Promise<ExtensionSettings>
  prepareBrowser(): Promise<void>
  chromeConnectionStatus(): Promise<NonNullable<BundledPlugin['chrome']>>
  connectChrome(mode?: 'store' | 'development' | 'repair'): Promise<void>
  openComputerSettings(permission: 'accessibility' | 'screen-recording'): Promise<void>
  extensionSettings(): Promise<ExtensionSettings>
  skillCatalog(workspacePath?: string): Promise<SkillCatalog>
  saveExtensions(config: ExtensionConfig): Promise<ExtensionSettings>
  installPlugin(
    expectedId?: string,
    sourceKind?: PluginSourceKind,
  ): Promise<ExtensionSettings | null>
  setPluginEnabled(id: string, enabled: boolean): Promise<ExtensionSettings>
  uninstallPlugin(id: string): Promise<ExtensionSettings>
  testMcp(id: string): Promise<McpCatalog>
  runExtensions(id: string): Promise<RunExtensions>
  runMemory(id: string): Promise<RunMemory>
  refreshRunSkills(id: string): Promise<RunExtensions>
  approveExtension(
    id: string,
    approvalId: string,
    decision: ControlApprovalDecision,
  ): Promise<RunExtensions>
  revokeControl(id: string): Promise<RunExtensions>
  revokeControlPermission(serverId: string): Promise<ExtensionSettings>
  openSkillsDirectory(): Promise<void>
  resolveResource(reference: ResourceReference): Promise<ResolvedResource>
  resourceAction(id: string, action: ResourceAction): Promise<void>
  openExternal(url: string): Promise<void>
  copyText(text: string): Promise<void>
  config(): Promise<AppConfig>
  listRuns(): Promise<RunSummary[]>
  getRun(id: string): Promise<RunDetail>
  createRun(input: CreateRunInput): Promise<RunDetail>
  updateRunModel(id: string, input: UpdateRunModelInput): Promise<RunDetail>
  chooseFiles(): Promise<FileImportResult>
  importFiles(files: File[]): Promise<FileImportResult>
  pasteFiles(files?: File[]): Promise<FileImportResult>
  chooseWorkspace(): Promise<string | null>
  openWorkspace(path: string): Promise<string>
  revealWorkspace(id: string): Promise<void>
  getWorkspace(id: string): Promise<WorkspaceReview>
  approveCommand(
    id: string,
    approvalId: string,
    allow: boolean,
    scope?: CommandApprovalScope,
  ): Promise<WorkspaceReview>
  revokeCommandGrants(id: string): Promise<WorkspaceReview>
  stopCommand(id: string, commandId: string): Promise<WorkspaceReview>
  revertFile(id: string, path: string): Promise<WorkspaceReview>
  resumeRun(id: string, message?: string, files?: FileReference[]): Promise<RunDetail>
  cancelRun(id: string): Promise<RunDetail>
  compactRun(id: string): Promise<RunDetail>
  cancelCompaction(id: string): Promise<RunDetail>
  configureModel(input: ConfigureModelInput): Promise<ConfigureModelResult>
  getModelConfiguration(id: string): Promise<ModelConfiguration>
  updateModelConfiguration(id: string, input: UpdateModelInput): Promise<ConfigureModelResult>
  refreshModelContext(id: string): Promise<ConfigureModelResult>
  submitInput(id: string, message: string, files?: FileReference[]): Promise<RunDetail>
  watchRun(
    id: string,
    listener: (snapshot: RunDetail) => void,
    onError: (message: string) => void,
  ): () => void
  platform: string
}

declare global {
  interface Window {
    harness: DesktopBridge
  }
}
