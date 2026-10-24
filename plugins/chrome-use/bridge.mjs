import net from 'node:net';
import { createHmac, randomBytes, randomUUID } from 'node:crypto';
import { readConfig } from './config.mjs';
import { LineDecoder } from './framing.mjs';
import { MAX_RESPONSE } from './constants.mjs';
import { AUTH_VERSION, authProof, isAuthValue, verifyAuthProof } from './auth.mjs';
export class ChromeBridge {
  constructor({ configPath, timeoutMs = 25000, taskId } = {}) {
    if (taskId !== undefined && (typeof taskId !== 'string' || !taskId.length || taskId.length > 256 || taskId.includes('\0'))) throw new Error('Invalid Chrome task identity.');
    this.configPath = configPath; this.timeoutMs = timeoutMs; this.taskId = taskId; this.pending = new Map(); this.sessionId = randomUUID(); this.nextId = 0;
  }
  async connect() {
    if (this.connecting) return this.connecting;
    if (this.socket && !this.socket.destroyed) return;
    this.connecting = this.open().finally(() => { this.connecting = undefined; });
    return this.connecting;
  }
  async open() {
    const config = await readConfig(this.configPath);
    const socket = net.createConnection(config.socketPath); this.socket = socket;
    socket.setEncoding('utf8');
    const decoder = new LineDecoder(message => {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id); clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(message.error)); else pending.resolve(message.result);
    }, MAX_RESPONSE);
    socket.on('data', chunk => { try { decoder.push(chunk); } catch { socket.destroy(new Error('Invalid Chrome bridge response.')); } });
    socket.on('error', () => { if (this.socket === socket) this.fail('Chrome extension is disconnected. Open its toolbar popup and connect again.'); });
    socket.on('close', () => { if (this.socket === socket) { this.socket = undefined; this.fail('Chrome extension disconnected; observe before retrying any action.'); } });
    try {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => { socket.destroy(); reject(new Error('Chrome extension connection timed out.')); }, 3000);
        socket.once('connect', () => { clearTimeout(timer); resolve(); });
        socket.once('error', () => { clearTimeout(timer); reject(new Error('Chrome extension is not connected. Open Chrome and connect from the extension popup.')); });
      });
      // Authenticate the server first: another local account could pre-create a
      // named pipe. Never send it the secret, a client proof or task metadata.
      const clientNonce = randomBytes(32).toString('hex');
      const hello = await this.dispatch('hello', { client_nonce: clientNonce });
      const context = { sessionId: this.sessionId, clientNonce, serverNonce: hello?.server_nonce };
      if (hello?.protocol_version !== AUTH_VERSION || !isAuthValue(context.serverNonce) || !verifyAuthProof(hello?.proof, config.token, 'server', context)) throw new Error('Chrome connection identity could not be verified. Repair its connection in Open Harness.');
      // Reconnecting a task reuses its visual group, not its old transport grants.
      // Disclose its opaque layout key only after verifying the host's identity.
      const taskKey = this.taskId === undefined ? undefined : createHmac('sha256', config.token).update(`openharness.chrome.task\0${this.taskId}`).digest('hex');
      context.taskKey = taskKey;
      const accepted = await this.dispatch('authenticate', {
        client_nonce: clientNonce,
        server_nonce: context.serverNonce,
        ...(taskKey ? { task_key: taskKey } : {}),
        proof: authProof(config.token, 'client', context),
      });
      if (accepted?.connected !== true || !verifyAuthProof(accepted?.proof, config.token, 'accepted', context)) throw new Error('Chrome connection authentication was not confirmed.');
    } catch (error) {
      socket.destroy();
      if (this.socket === socket) this.socket = undefined;
      this.fail('Chrome connection authentication failed.');
      throw error;
    }
  }
  dispatch(method, params) {
    return new Promise((resolve, reject) => {
      const id = ++this.nextId;
      const timer = setTimeout(() => { this.pending.delete(id); this.socket?.destroy(); reject(new Error('Chrome operation timed out; its effect may be uncertain. Reconnect and observe before retrying.')); }, this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.write(`${JSON.stringify({ id, session_id: this.sessionId, method, params })}\n`);
    });
  }
  async request(method, params = {}) { await this.connect(); return this.dispatch(method, params); }
  fail(message) { for (const item of this.pending.values()) { clearTimeout(item.timer); item.reject(new Error(message)); } this.pending.clear(); }
  close() { this.socket?.destroy(); this.fail('Chrome session closed.'); }
}
