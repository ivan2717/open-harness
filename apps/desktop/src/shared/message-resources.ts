/** Presentation references; these do not change model input or filesystem permissions. */
export type ResourceReference = { runId: string; messageId: string } & (
  { source: string; fileIndex?: never } | { fileIndex: number; source?: never }
)

export type ResourceKind = 'image' | 'video' | 'audio' | 'text' | 'file'
export type ResourceAction = 'open' | 'reveal' | 'save'

export interface ResolvedResource {
  id: string
  name: string
  mimeType: string
  size: number
  url: string
  kind: ResourceKind
  canOpen: boolean
  text?: string
  sourcePath?: string
}

/** A bare filename with a line/column suffix can otherwise resemble a URL scheme. */
export function isFileLocation(source: string): boolean {
  return /^[^:/\\?#]+\.[^:/\\?#]+:\d+(?::\d+)?$/.test(source)
}
