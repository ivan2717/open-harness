import { spawn } from 'node:child_process';
import { AppTransport } from './app-transport.mjs';

// One helper belongs to one MCP session. No commands are replayed after failures.
export class ComputerBridge {
  constructor(command, args = [], { timeoutMs = 15_000 } = {}) {
    this.command = command;
    this.args = args;
    this.timeoutMs = timeoutMs;
    this.child = null;
    this.pending = null;
    this.sequence = 0;
    this.generation = 0;
    this.tail = Promise.resolve();
  }
  request(method, params, signal) {
    const generation = this.generation;
    const operation = this.tail.then(() => {
      if (generation !== this.generation || signal?.aborted) throw new Error('Session cancelled; select the app again. The previous action may already have been dispatched.');
      return this.execute(method, params, signal);
    });
    this.tail = operation.catch(() => {});
    return operation;
  }
  execute(method, params, signal) {
    if (!this.child) this.start();
    return new Promise((resolve, reject) => {
      const id = ++this.sequence;
      const timer = setTimeout(() => this.stop('Computer operation timed out. State is unknown; no action was replayed. Select the app again.'), this.timeoutMs);
      const abort = () => this.stop('Computer operation cancelled. An action may already have been dispatched; inspect the app before continuing.');
      const cleanup = () => { clearTimeout(timer); signal?.removeEventListener('abort', abort); };
      this.pending = { id, resolve: result => { cleanup(); resolve(result); }, reject: error => { cleanup(); reject(error); } };
      signal?.addEventListener('abort', abort, { once: true });
      if (signal?.aborted) return abort();
      this.child.stdin.write(`${JSON.stringify({ id, method, params })}\n`, error => {
        if (error) this.stop('Computer helper connection failed; select the app again.');
      });
    });
  }
  start() {
    const child = this.command.endsWith('.app')
      ? new AppTransport(this.command)
      : spawn(this.command, this.args, { stdio: ['pipe', 'pipe', 'pipe'] });
    this.child = child;
    // Helper diagnostics never contain model-visible application text.
    child.stderr.resume();
    child.stdout.setEncoding('utf8');
    let buffer = '';
    child.stdout.on('data', chunk => {
      if (child !== this.child) return;
      buffer += chunk;
      if (buffer.length > 8 * 1024 * 1024) return this.stop('Computer helper response exceeded 8 MiB.');
      let end;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
        let message;
        try { message = JSON.parse(line); } catch { return this.stop('Invalid computer helper response.'); }
        if (child !== this.child || !this.pending || message.id !== this.pending.id) return this.stop('Mismatched computer helper response.');
        const pending = this.pending; this.pending = null;
        if (message.error) pending.reject(new Error(`${message.error.code}: ${message.error.message}`));
        else if (message.result && typeof message.result === 'object') pending.resolve(message.result);
        else { pending.reject(new Error('Invalid computer helper result.')); this.stop(); }
      }
    });
    child.once('error', () => {
      if (this.child === child) this.stop('Computer helper could not start. Build the macOS plugin with node build.mjs before installation.');
    });
    child.once('exit', () => {
      if (this.child === child) this.stop('Computer helper exited. State is unknown; select the app again.');
    });
  }
  stop(reason = 'Computer session released.') {
    this.generation++;
    const child = this.child;
    this.child = null;
    const pending = this.pending;
    this.pending = null;
    pending?.reject(new Error(reason));
    if (child) {
      child.stdin.destroy();
      child.kill('SIGTERM');
      const force = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); }, 500);
      force.unref();
    }
  }
}
