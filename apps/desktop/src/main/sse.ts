export interface ServerEvent {
  event: string
  data: string
}

/** Incremental SSE framing, including CRLF and multiline data split across network chunks. */
export class SseDecoder {
  private pending = ''
  private event = ''
  private data: string[] = []
  private size = 0

  push(chunk: string): ServerEvent[] {
    this.pending += chunk
    if (this.pending.length + this.size > 16 * 1024 * 1024)
      throw new Error('Server event exceeds size limit')
    const events: ServerEvent[] = []
    for (;;) {
      const newline = this.pending.indexOf('\n')
      if (newline < 0) break
      const line = this.pending.slice(0, newline).replace(/\r$/, '')
      this.pending = this.pending.slice(newline + 1)
      if (!line) {
        if (this.data.length)
          events.push({ event: this.event || 'message', data: this.data.join('\n') })
        this.event = ''
        this.data = []
        this.size = 0
      } else if (!line.startsWith(':')) {
        const colon = line.indexOf(':')
        const field = colon < 0 ? line : line.slice(0, colon)
        const value = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '')
        if (field === 'event') this.event = value
        else if (field === 'data') {
          this.data.push(value)
          this.size += value.length
        }
      }
    }
    return events
  }
}

export async function consumeSse(
  response: Response,
  signal: AbortSignal,
  receive: (event: ServerEvent) => void,
): Promise<void> {
  if (!response.ok) throw new Error(`Event connection failed (${response.status})`)
  if (!response.headers.get('content-type')?.includes('text/event-stream'))
    throw new Error('Invalid event stream response')
  if (!response.body) throw new Error('Empty event stream')
  const reader = response.body.getReader()
  const utf8 = new TextDecoder()
  const decoder = new SseDecoder()
  try {
    while (!signal.aborted) {
      const { done, value } = await reader.read()
      if (done) break
      for (const event of decoder.push(utf8.decode(value, { stream: true }))) receive(event)
    }
  } finally {
    await reader.cancel().catch(() => undefined)
    reader.releaseLock()
  }
}
