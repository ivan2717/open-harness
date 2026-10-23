import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { chmod, mkdtemp, realpath, rm, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

export const COMPUTER_APP_NAME = 'Open Harness Computer Use';
export const COMPUTER_APP_ID = 'dev.openharness.computeruse';

// LaunchServices, rather than a directly spawned child executable, owns this app.
// The private, authenticated Unix channel carries the same protocol as stdio.
export class AppTransport extends EventEmitter {
  constructor(appPath, { launch = spawn, startupTimeoutMs = 10_000 } = {}) {
    super();
    this.stdin = new PassThrough();
    this.stdout = new PassThrough();
    this.stderr = new PassThrough();
    this.exitCode = null;
    this.signalCode = null;
    this.closed = false;
    this.socket = null;
    this.clients = new Set();
    this.server = null;
    this.launcher = null;
    this.directory = null;
    this.timer = setTimeout(() => this.fail('Computer Use app did not connect in time.'), startupTimeoutMs);
    this.stdin.on('error', () => this.kill());
    void this.start(appPath, launch).catch(error => this.fail(error.message));
  }

  async start(appPath, launch) {
    if (!path.isAbsolute(appPath) || !appPath.endsWith('.app')) throw new Error('Computer Use requires an absolute .app path.');
    const directory = await mkdtemp(path.join(await realpath(tmpdir()), 'ohcu-'));
    this.directory = directory;
    await chmod(directory, 0o700);
    const nonce = randomBytes(32).toString('hex');
    await writeFile(path.join(directory, 'token'), nonce, { flag: 'wx', mode: 0o600 });
    const socketPath = path.join(directory, 'control.sock');
    if (Buffer.byteLength(socketPath) >= 104) throw new Error('Computer Use socket path is too long.');
    if (this.closed) return this.cleanup();
    const server = createServer(socket => this.authenticate(socket, nonce));
    this.server = server;
    server.on('error', error => this.fail(`Computer Use local connection failed: ${error.message}`));
    await new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(socketPath, () => { server.off('error', reject); resolve(); });
    });
    await chmod(socketPath, 0o600);
    if (this.closed) return this.cleanup();
    this.launcher = launch('/usr/bin/open', ['-g', '-j', '-n', '-W', appPath, '--args', '--socket-dir', directory], { stdio: 'ignore' });
    this.launcher.once('error', error => this.fail(`Computer Use app could not start: ${error.message}`));
    this.launcher.once('exit', (code, signal) => {
      if (!this.closed) this.fail(`Computer Use app exited (${signal ?? code ?? 'unknown'}).`);
    });
  }

  authenticate(socket, nonce) {
    if (this.closed || this.socket) { socket.destroy(); return; }
    this.clients.add(socket);
    socket.once('close', () => this.clients.delete(socket));
    let buffered = Buffer.alloc(0);
    socket.setTimeout(2_000, () => socket.destroy());
    socket.on('error', () => socket.destroy());
    const onData = chunk => {
      buffered = Buffer.concat([buffered, chunk]);
      const newline = buffered.indexOf(10);
      if (newline < 0 && buffered.length <= 4096) return;
      let hello;
      try { if (newline < 0 || newline > 4096) throw new Error(); hello = JSON.parse(buffered.subarray(0, newline).toString('utf8')); } catch { socket.destroy(); return; }
      const provided = Buffer.from(typeof hello.token === 'string' ? hello.token : '');
      const expected = Buffer.from(nonce);
      if (hello.protocol !== 1 || hello.bundle_id !== COMPUTER_APP_ID || !Number.isSafeInteger(hello.pid) || hello.pid <= 0 || provided.length !== expected.length || !timingSafeEqual(provided, expected) || this.socket || this.closed) {
        socket.destroy(); return;
      }
      clearTimeout(this.timer);
      socket.setTimeout(0);
      socket.off('data', onData);
      this.socket = socket;
      this.pid = hello.pid;
      void unlink(path.join(this.directory, 'token')).catch(() => {});
      this.server.close();
      socket.once('close', () => this.kill());
      this.stdin.pipe(socket);
      socket.pipe(this.stdout);
      if (buffered.length > newline + 1) this.stdout.write(buffered.subarray(newline + 1));
    };
    socket.on('data', onData);
  }

  fail(message) {
    if (this.closed) return;
    this.emit('error', new Error(message));
    this.kill();
  }

  kill() {
    if (this.closed) return false;
    this.closed = true;
    clearTimeout(this.timer);
    this.stdin.destroy();
    this.socket?.destroy();
    for (const socket of this.clients) socket.destroy();
    // The native app exits on channel EOF, including while awaiting AppKit.
    // Killing open alone is not used as a substitute for terminating the app.
    this.launcher?.kill('SIGTERM');
    this.stdout.end();
    this.stderr.end();
    this.exitCode = 0;
    void this.cleanup();
    this.emit('exit', 0, null);
    return true;
  }

  async cleanup() {
    this.server?.close();
    if (this.directory) await rm(this.directory, { recursive: true, force: true }).catch(() => {});
  }
}
