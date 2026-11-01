import { afterEach, describe, expect, it } from 'vitest'
import { createConnection, type Socket } from 'node:net'
import { ComputerPreviewHost, previewPacket } from './computer-preview'
import type { ComputerPreview } from '../shared/computer-preview'

const cleanups: (() => void)[] = []
afterEach(() => cleanups.splice(0).forEach((fn) => fn()))
const delay = () => new Promise((resolve) => setTimeout(resolve, 20))
async function until(test: () => boolean) {
  for (let i = 0; i < 100; i++) {
    if (test()) return
    await delay()
  }
  throw new Error('Preview condition timed out')
}
async function fixture() {
  const events: ComputerPreview[] = []
  const host = new ComputerPreviewHost((event) => events.push(event))
  const [port, token] = (await host.start()).split(':')
  cleanups.push(() => host.stop())
  const connect = (auth = token) => {
    const controls: { enabled: boolean }[] = []
    const socket = createConnection({ host: '127.0.0.1', port: Number(port) })
    let tail = ''
    socket.setEncoding('utf8')
    socket.on('error', () => {})
    socket.on('data', (chunk) => {
      tail += chunk
      let end
      while ((end = tail.indexOf('\n')) >= 0) {
        controls.push(JSON.parse(tail.slice(0, end)))
        tail = tail.slice(end + 1)
      }
    })
    socket.write(JSON.stringify({ token: auth }) + '\n')
    cleanups.push(() => socket.destroy())
    return { socket, controls }
  }
  return { host, events, connect }
}
const source = { active: true, title: 'Fixture', width: 640, height: 480 }
const send = (socket: Socket, value: unknown) => socket.write(JSON.stringify(value) + '\n')

describe('local computer preview channel', () => {
  it('rejects unauthenticated peers without publishing or enabling capture', async () => {
    const f = await fixture()
    const peer = f.connect('invalid')
    await until(() => peer.socket.destroyed)
    expect(f.events).toEqual([])
    expect(peer.controls).toEqual([])
  })
  it('pauses hidden/collapsed capture, discards frame caches, and clears disconnected sources', async () => {
    const f = await fixture()
    const peer = f.connect()
    await until(() => peer.controls.length === 1)
    expect(peer.controls[0].enabled).toBe(false)
    send(peer.socket, { ...source, jpeg: '/9j/abcd' })
    await until(() => f.events.length === 1)
    const id = f.events[0].id
    expect(f.events[0].jpeg).toBeUndefined()
    f.host.setVisible(true)
    await until(() => peer.controls.at(-1)?.enabled === true)
    send(peer.socket, { ...source, jpeg: '/9j/abcd' })
    await until(() => f.events.at(-1)?.jpeg === '/9j/abcd')
    expect(f.host.snapshot()[0].jpeg).toBeUndefined()
    f.host.setCollapsed(id, true)
    await until(() => peer.controls.at(-1)?.enabled === false)
    send(peer.socket, { active: true, title: 'Background browser' })
    await until(() => f.events.at(-1)?.title === 'Background browser')
    expect(peer.socket.destroyed).toBe(false)
    expect(f.host.snapshot()[0]).toMatchObject({ id, active: true, width: 640, height: 480 })
    expect(f.events.at(-1)?.jpeg).toBeUndefined()
    f.host.setVisible(false)
    f.host.setCollapsed(id, false)
    await delay()
    expect(peer.controls.at(-1)?.enabled).toBe(false)
    f.host.setVisible(true)
    await until(() => peer.controls.at(-1)?.enabled === true)
    peer.socket.destroy()
    await until(() => f.events.at(-1)?.active === false)
    expect(f.host.snapshot()).toEqual([])
  })
  it('bounds frames and coordinates and never accepts supplied source ids', () => {
    expect(previewPacket({ ...source, id: 'forged' }, 'assigned').id).toBe('assigned')
    for (const invalid of [
      { ...source, width: Infinity },
      { active: true, title: 'Partial dimensions', width: 640 },
      { active: true, title: 'Frame without dimensions', jpeg: '/9j/abcd' },
      { ...source, jpeg: 'file:///private' },
      { ...source, jpeg: '/9j/' + 'a'.repeat(800000) },
      { ...source, pointer: { x: -1, y: 1, pressed: true, sequence: 1 } },
    ]) {
      expect(() => previewPacket(invalid, 'id')).toThrow()
    }
  })
  it('accepts metadata before the first frame or while capture is unavailable', async () => {
    const f = await fixture()
    const peer = f.connect()
    await until(() => peer.controls.length === 1)
    send(peer.socket, { active: true, title: 'Chrome', unavailable: true })
    await until(() => f.events.length === 1)
    expect(f.events[0]).toMatchObject({ active: true, title: 'Chrome', unavailable: true })
    expect(peer.socket.destroyed).toBe(false)
    f.host.setVisible(true)
    send(peer.socket, { ...source, jpeg: '/9j/abcd' })
    await until(() => f.events.at(-1)?.jpeg === '/9j/abcd')
    expect(f.host.snapshot()[0].unavailable).toBe(false)
  })
})
