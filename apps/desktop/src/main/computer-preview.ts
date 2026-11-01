import { randomBytes, randomUUID, timingSafeEqual } from 'node:crypto'
import { createServer, type Server, type Socket } from 'node:net'
import type { ComputerPreview } from '../shared/computer-preview'

const MAX_PACKET = 900_000
type Peer = { id: string; socket: Socket; collapsed: boolean; state?: ComputerPreview }

/** Local authenticated display channel, separate from model/MCP output. */
export class ComputerPreviewHost {
  private server?: Server
  private sockets = new Set<Socket>()
  private peers = new Map<string, Peer>()
  private visible = false
  private token = randomBytes(32).toString('hex')
  constructor(private publish: (state: ComputerPreview) => void) {}

  async start(): Promise<string> {
    this.server = createServer((socket) => this.accept(socket))
    await new Promise<void>((resolve, reject) => {
      this.server!.once('error', reject)
      this.server!.listen(0, '127.0.0.1', resolve)
    })
    const address = this.server.address()
    if (!address || typeof address === 'string') throw new Error('Preview channel unavailable')
    return `${address.port}:${this.token}`
  }

  private accept(socket: Socket) {
    if (this.sockets.size >= 8) {
      socket.destroy()
      return
    }
    this.sockets.add(socket)
    let peer: Peer | undefined,
      buffer = ''
    socket.setEncoding('utf8')
    socket.setTimeout(3000, () => socket.destroy())
    socket.on('error', () => socket.destroy())
    socket.on('close', () => {
      this.sockets.delete(socket)
      if (peer) {
        this.peers.delete(peer.id)
        this.publish({ id: peer.id, active: false })
      }
    })
    socket.on('data', (chunk: string) => {
      buffer += chunk
      if (buffer.length > MAX_PACKET) {
        socket.destroy()
        return
      }
      let end: number
      while ((end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, end)
        buffer = buffer.slice(end + 1)
        try {
          const value = JSON.parse(line)
          if (!peer) {
            const supplied = Buffer.from(typeof value.token === 'string' ? value.token : '')
            const expected = Buffer.from(this.token)
            if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected))
              throw new Error()
            peer = { id: randomUUID(), socket, collapsed: false }
            this.peers.set(peer.id, peer)
            socket.setTimeout(0)
            this.control(peer)
          } else {
            const state = previewPacket(value, peer.id)
            if (state.active) {
              peer.state = {
                ...peer.state,
                ...state,
                ...(state.jpeg ? { unavailable: false } : {}),
              }
              // Retain metadata only. No frame cache while collapsed/hidden.
              if (this.visible && !peer.collapsed) this.publish(state)
              else this.publish({ ...state, jpeg: undefined, pointer: undefined })
              peer.state.jpeg = undefined
              peer.state.pointer = undefined
            } else {
              peer.state = undefined
              this.publish(state)
            }
          }
        } catch {
          socket.destroy()
          return
        }
      }
    })
  }

  private control(peer: Peer) {
    if (peer.socket.writableLength > 4096) {
      peer.socket.destroy()
      return
    }
    peer.socket.write(`${JSON.stringify({ enabled: this.visible && !peer.collapsed })}\n`)
  }
  setVisible(visible: boolean) {
    if (this.visible === visible) return
    this.visible = visible
    for (const peer of this.peers.values()) this.control(peer)
  }
  setCollapsed(id: unknown, collapsed: unknown) {
    if (typeof id !== 'string' || typeof collapsed !== 'boolean') throw new Error('无效的预览设置')
    const peer = this.peers.get(id)
    if (peer) {
      peer.collapsed = collapsed
      this.control(peer)
    }
  }
  snapshot(): ComputerPreview[] {
    return [...this.peers.values()].flatMap((peer) => (peer.state ? [peer.state] : []))
  }
  stop() {
    for (const socket of this.sockets) socket.destroy()
    this.server?.close()
    this.peers.clear()
  }
}

export function previewPacket(value: unknown, id: string): ComputerPreview {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Invalid preview')
  const v = value as Record<string, unknown>
  if (typeof v.active !== 'boolean') throw new Error('Invalid preview state')
  if (!v.active) return { id, active: false }
  const dimension = (n: unknown) =>
    typeof n === 'number' && Number.isFinite(n) && n > 0 && n <= 32768
  const hasDimensions = v.width !== undefined || v.height !== undefined
  if (
    typeof v.title !== 'string' ||
    v.title.length > 256 ||
    (hasDimensions && (!dimension(v.width) || !dimension(v.height))) ||
    ((v.jpeg !== undefined || v.pointer !== undefined) && !hasDimensions)
  )
    throw new Error('Invalid preview source')
  const state: ComputerPreview = {
    id,
    active: true,
    title: v.title,
    // Hidden browsers send only target metadata; keep the existing card without
    // requiring another screenshot just to recover its dimensions.
    ...(hasDimensions ? { width: v.width as number, height: v.height as number } : {}),
  }
  if (v.jpeg !== undefined) {
    if (
      typeof v.jpeg !== 'string' ||
      v.jpeg.length > 800_000 ||
      !/^\/9j\/[A-Za-z0-9+/=]+$/.test(v.jpeg)
    )
      throw new Error('Invalid preview frame')
    state.jpeg = v.jpeg
  }
  if (v.unavailable === true) state.unavailable = true
  if (v.pointer !== undefined) {
    const p = v.pointer as Record<string, unknown>
    if (
      !p ||
      typeof p.x !== 'number' ||
      typeof p.y !== 'number' ||
      !Number.isFinite(p.x) ||
      !Number.isFinite(p.y) ||
      p.x < 0 ||
      p.y < 0 ||
      p.x > state.width! ||
      p.y > state.height! ||
      typeof p.pressed !== 'boolean' ||
      !Number.isSafeInteger(p.sequence)
    )
      throw new Error('Invalid preview pointer')
    state.pointer = p as ComputerPreview['pointer']
  }
  return state
}
