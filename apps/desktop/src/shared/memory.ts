/** A read-only, derived memory of this run. Source events remain authoritative. */
export interface RunMemory {
  enabled: boolean
  status: 'empty' | 'ready' | 'error' | 'disabled'
  revision: number
  through_sequence: number | null
  source_cursor?: { sequence: number; offset: number } | null
  pending?: boolean
  overview: string
  pages: MemoryPage[]
  tasks?: MemoryTask[]
  error: string | null
}

export interface MemoryTask {
  id: string
  kind: 'goal' | 'constraint' | 'decision' | 'work' | 'question'
  status: 'active' | 'planned' | 'attempted' | 'completed' | 'failed' | 'superseded' | 'uncertain'
  text: string
  evidence: { sequence: number; quote: string }[]
}

export interface MemoryPage {
  id: string
  title: string
  content: string
  source_sequences: number[]
  updated_through: number
}
