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
    private environment: NodeJS.ProcessEnv = {},
  ) {}

  start(): Promise<string> {
    if (this.ready) return this.ready
    this.ready = new Promise((resolve, reject) => {
      const child = spawn(this.binary, [], {
        env: {
          ...process.env,
          ...this.environment,
          OPENHARNESS_BIND: '127.0.0.1:0',
          OPENHARNESS_DATA_DIR: this.dataDirectory,
          OPENHARNESS_API_TOKEN: this.token,
          OPENHARNESS_EXIT_ON_STDIN_EOF: '1',
        },
        stdio: 'pipe',
        windowsHide: true,
      })
      this.child = child
      let settled = false
      let output = ''
      const timer = setTimeout(() => {
        if (settled) return
        this.failure = new Error(
          'The local runtime did not become ready within 30 seconds. 请重新启动 Open Harness。',
        )
        settled = true
        reject(this.failure)
        child.kill()
      }, 30_000)
      const fail = (error: Error): void => {
        // Terminating a timed-out startup can produce exit(0). Keep its cause,
        // rather than replacing it with a misleading clean-exit message.
        this.failure ??= error
        clearTimeout(timer)
        if (!settled) {
          settled = true
          reject(this.failure)
        }
      }
      child.on('error', () =>
        fail(
          new Error(
            'Could not start the local runtime. 请重新启动 Open Harness；若仍失败，请重新构建或安装应用。',
          ),
        ),
      )
      child.stdin.on('error', () => {
        if (!this.stopping) fail(new Error('The local runtime control channel closed'))
      })
      child.on('exit', (code, signal) => {
        if (!this.stopping)
          fail(
            new Error(
              `The local runtime stopped (${signal ?? code ?? 'unknown'}). 请重新启动 Open Harness。`,
            ),
          )
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
          fail(new Error('Invalid local runtime startup response. 请重新启动 Open Harness。'))
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
              fail(new Error('Invalid local runtime address. 请重新启动 Open Harness。'))
              child.kill()
              return
            }
            const port = Number(candidate.address.split(':')[1])
            if (port < 1 || port > 65535) {
              fail(new Error('Invalid local runtime port. 请重新启动 Open Harness。'))
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
    // MCP discovery has a 60-second runtime deadline plus bounded cleanup.
    // Leave room for its result without extending ordinary local API requests.
    const timeout =
      request.method === 'POST' &&
      /^\/api\/extensions\/mcp\/[a-zA-Z0-9_-]{1,128}\/test$/.test(request.path)
        ? 70_000
        : 30_000
    let response: Response
    try {
      response = await fetch(`${origin}${request.path}`, {
        method: request.method,
        headers: { Authorization: `Bearer ${this.token}`, 'Content-Type': 'application/json' },
        body: request.body === undefined ? undefined : JSON.stringify(request.body),
        signal: AbortSignal.timeout(timeout),
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
      // Closing our pipe is also effective when the app crashes. Windows does
      // not deliver SIGTERM as a graceful signal; keep hard kill as a deadline.
      child.stdin.end()
    })
  }
}
