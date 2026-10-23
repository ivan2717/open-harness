import type { ModelCapacity, ResolvedModelLimits } from './model-limits'
import type {
  ExtensionConfig,
  ExtensionSettings,
  McpCatalog,
  RunExtensions,
  PluginSourceKind,
  SkillCatalog,
} from './extensions'
import type { ResolvedResource, ResourceAction, ResourceReference } from './message-resources'
import type { RunMemory } from './memory'

export type RunStatus =
  'running' | 'waiting_for_input' | 'completed' | 'failed' | 'cancelled' | 'interrupted'

export type PermissionMode = 'read_only' | 'workspace_write'

export interface ModelInfo {
  id: string
  name: string
  description: string
  available: boolean
  context_window: number
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
  reasoning_effort: 'low' | 'medium' | 'high'
}

export type ResolvedModelConfiguration = Omit<
  ConfigureModelInput,
  'context_window' | 'max_output_tokens'
> & {
  context_window: number
  max_output_tokens: number
}

export interface ConfigureModelResult {
  model: ModelInfo
  persisted: boolean
  limits: ResolvedModelLimits
  capacity?: ModelCapacity
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
  model_id: string
  workspace_path?: string | null
  permission_mode?: PermissionMode
  created_at: string
  updated_at: string
}

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant' | 'tool'
  text: string
  files?: FileReferenceInfo[]
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
  files?: TextFileReference[]
  workspace_path?: string
  permission_mode?: PermissionMode
}

/** A user-selected UTF-8 file snapshot, not a grant to its parent directory. */
export interface TextFileReference {
  name: string
  path?: string
  text: string
}

export interface FileReferenceInfo {
  name: string
  path?: string
  size: number
}

export interface FileImportResult {
  files: TextFileReference[]
  errors: string[]
}

export interface FileChange {
  path: string
  status: string
  before: string | null
  after: string | null
  revertible: boolean
}

export interface PendingApproval {
  id: string
  command: string
  cwd: string
}

export interface CommandRecord {
  id: string
  command: string
  status: string
  output: string
  exit_code: number | null
}

export interface WorkspaceReview {
  root: string
  permission_mode: PermissionMode
  busy: boolean
  sandbox_available: boolean
  review_incomplete?: boolean
  changes: FileChange[]
  approvals: PendingApproval[]
  commands: CommandRecord[]
}

export interface DesktopBridge {
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
  approveExtension(id: string, approvalId: string, allow: boolean): Promise<RunExtensions>
  openSkillsDirectory(): Promise<void>
  resolveResource(reference: ResourceReference): Promise<ResolvedResource>
  resourceAction(id: string, action: ResourceAction): Promise<void>
  openExternal(url: string): Promise<void>
  copyText(text: string): Promise<void>
  config(): Promise<AppConfig>
  listRuns(): Promise<RunSummary[]>
  getRun(id: string): Promise<RunDetail>
  createRun(input: CreateRunInput): Promise<RunDetail>
  chooseFiles(): Promise<FileImportResult>
  importFiles(files: File[]): Promise<FileImportResult>
  pasteFiles(): Promise<FileImportResult>
  chooseWorkspace(): Promise<string | null>
  openWorkspace(path: string): Promise<string>
  getWorkspace(id: string): Promise<WorkspaceReview>
  approveCommand(id: string, approvalId: string, allow: boolean): Promise<WorkspaceReview>
  revertFile(id: string, path: string): Promise<WorkspaceReview>
  resumeRun(id: string, message?: string, files?: TextFileReference[]): Promise<RunDetail>
  cancelRun(id: string): Promise<RunDetail>
  compactRun(id: string): Promise<RunDetail>
  cancelCompaction(id: string): Promise<RunDetail>
  configureModel(input: ConfigureModelInput): Promise<ConfigureModelResult>
  submitInput(id: string, message: string, files?: TextFileReference[]): Promise<RunDetail>
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
