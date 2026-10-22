import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import type { ApiRequest } from './protocol'
import { consumeSse } from './sse'
import type { RunDetail } from '../shared/contracts'

export class Backend {
  private child: ChildProcessWithoutNullStreams | undefined
  private ready: Promise<string> | undefined
  private failure: Error | undefined
  private stopping = false
  private token = randomBytes(32).toString('hex')

  constructor(
    private binary: string,
    private dataDirectory: string,
  ) {}

  start(): Promise<string> {
    if (this.ready) return this.ready
    this.ready = new Promise((resolve, reject) => {
      const child = spawn(this.binary, [], {
        env: {
          ...process.env,
          OPENHARNESS_BIND: '127.0.0.1:0',
          OPENHARNESS_DATA_DIR: this.dataDirectory,
          OPENHARNESS_API_TOKEN: this.token,
        },
        stdio: 'pipe',
        windowsHide: true,
      })
      this.child = child
      let settled = false
      let output = ''
      const timer = setTimeout(() => {
        if (settled) return
        this.failure = new Error('The local runtime did not become ready within 30 seconds')
        settled = true
        reject(this.failure)
        child.kill()
      }, 30_000)
      const fail = (error: Error): void => {
        this.failure = error
        clearTimeout(timer)
        if (!settled) {
          settled = true
          reject(error)
        }
      }
      child.on('error', () =>
        fail(
          new Error(
            'Could not start the local runtime. Build the Rust API binary and restart the app.',
          ),
        ),
      )
      child.on('exit', (code, signal) => {
        if (!this.stopping)
          fail(new Error(`The local runtime stopped (${signal ?? code ?? 'unknown'})`))
        else {
          clearTimeout(timer)
          if (!settled) {
            settled = true
            reject(new Error('Runtime stopped'))
          }
        }
      })
      child.stderr.on('data', (chunk: Buffer) => process.stderr.write(chunk))
      child.stdout.on('data', (chunk: Buffer) => {
        if (settled) return
        output += chunk.toString('utf8')
        if (output.length > 64_000) {
          fail(new Error('Invalid local runtime startup response'))
          child.kill()
          return
        }
        const lines = output.split('\n')
        output = lines.pop() ?? ''
        for (const line of lines) {
          try {
            const event: unknown = JSON.parse(line)
            if (!event || typeof event !== 'object') continue
            const candidate = event as Record<string, unknown>
            if (candidate.event !== 'ready') continue
            if (
              typeof candidate.address !== 'string' ||
              !/^127\.0\.0\.1:\d{1,5}$/.test(candidate.address)
            ) {
              fail(new Error('Invalid local runtime address'))
              child.kill()
              return
            }
            const port = Number(candidate.address.split(':')[1])
            if (port < 1 || port > 65535) {
              fail(new Error('Invalid local runtime port'))
              child.kill()
              return
            }
            clearTimeout(timer)
            settled = true
            resolve(`http://${candidate.address}`)
            return
          } catch {
            /* Ignore ordinary startup logs; only the ready record establishes the address. */
          }
        }
      })
    })
    return this.ready
  }

  private async origin(): Promise<string> {
    if (this.failure) throw this.failure
    const origin = await this.start()
    if (this.failure) throw this.failure
    return origin
  }

  async request(request: ApiRequest): Promise<unknown> {
    const origin = await this.origin()
    let response: Response
    try {
      response = await fetch(`${origin}${request.path}`, {
        method: request.method,
        headers: { Authorization: `Bearer ${this.token}`, 'Content-Type': 'application/json' },
        body: request.body === undefined ? undefined : JSON.stringify(request.body),
        signal: AbortSignal.timeout(30_000),
        redirect: 'error',
      })
    } catch {
      throw new Error('Could not reach the local runtime')
    }
    const body: unknown = await response.json().catch(() => null)
    if (!response.ok) {
      const message =
        body && typeof body === 'object' && 'error' in body && typeof body.error === 'string'
          ? body.error
          : `Runtime request failed (${response.status})`
      throw new Error(message)
    }
    return body
  }

  async watch(
    id: string,
    signal: AbortSignal,
    receive: (snapshot: RunDetail) => void,
    onError: (message: string) => void,
  ): Promise<void> {
    let failures = 0
    while (!signal.aborted) {
      try {
        const origin = await this.origin()
        if (signal.aborted) return
        const response = await fetch(`${origin}/api/runs/${id}/events`, {
          headers: { Authorization: `Bearer ${this.token}`, Accept: 'text/event-stream' },
          signal,
          redirect: 'error',
        })
        await consumeSse(response, signal, (event) => {
          if (event.event !== 'snapshot' || signal.aborted) return
          const snapshot: unknown = JSON.parse(event.data)
          if (
            !snapshot ||
            typeof snapshot !== 'object' ||
            !('id' in snapshot) ||
            snapshot.id !== id
          )
            throw new Error('Invalid run event')
          failures = 0
          receive(snapshot as RunDetail)
        })
        if (!signal.aborted) throw new Error('Event connection closed')
      } catch {
        if (signal.aborted) return
        onError(this.failure?.message ?? 'Live connection interrupted; reconnecting…')
        if (this.failure) return
        const delay = Math.min(10_000, 500 * 2 ** Math.min(failures++, 5))
        await new Promise<void>((resolve) => {
          const done = (): void => {
            clearTimeout(timer)
            signal.removeEventListener('abort', done)
            resolve()
          }
          const timer = setTimeout(done, delay)
          signal.addEventListener('abort', done, { once: true })
          if (signal.aborted) done()
        })
      }
    }
  }

  async stop(): Promise<void> {
    this.stopping = true
    const child = this.child
    if (!child || child.exitCode !== null || child.signalCode !== null) return
    await new Promise<void>((resolve) => {
      // Let the runtime cancel owned tools, terminate their process groups and
      // persist the final run state before using the last-resort hard stop.
      const hardKill = setTimeout(() => child.kill('SIGKILL'), 10_000)
      const deadline = setTimeout(finish, 12_000)
      function finish(): void {
        clearTimeout(hardKill)
        clearTimeout(deadline)
        child?.removeListener('exit', finish)
        resolve()
      }
      child.once('exit', finish)
      child.kill('SIGTERM')
    })
  }
}
