export type RunStatus =
  'running' | 'waiting_for_input' | 'completed' | 'failed' | 'cancelled' | 'interrupted'

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
}

export interface RunSummary {
  id: string
  /** Monotonic snapshot revision, including transient stream updates. */
  revision: number
  title: string
  prompt: string
  status: RunStatus
  model_id: string
  created_at: string
  updated_at: string
}

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant' | 'tool'
  text: string
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
  measurement: 'estimated' | 'provider'
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
}

export interface DesktopBridge {
  config(): Promise<AppConfig>
  listRuns(): Promise<RunSummary[]>
  getRun(id: string): Promise<RunDetail>
  createRun(input: CreateRunInput): Promise<RunDetail>
  cancelRun(id: string): Promise<RunDetail>
  submitInput(id: string, message: string): Promise<RunDetail>
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
