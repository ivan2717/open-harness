import net from 'node:net';
import { chmod, lstat, unlink } from 'node:fs/promises';
import { randomBytes, randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { readConfig } from './config.mjs';
import { nativeFrame, NativeDecoder, LineDecoder } from './framing.mjs';
import { validate } from './schema.mjs';
import { validateSocketPath } from './endpoint.mjs';
import { AUTH_VERSION, authProof, isAuthValue, verifyAuthProof } from './auth.mjs';

// Chrome owns this process. Only a fixed extension origin and an authenticated
// local socket may reach it; websites have no external messaging entry point.
export async function startNativeHost({ config, input = process.stdin, output = process.stdout, origin = process.argv[2] } = {}) {
  config ??= await readConfig();
  validateSocketPath(config.socketPath);
  if (origin !== `chrome-extension://${config.extensionId}/`) throw new Error('Unexpected extension origin.');
  const clients = new Set(); const pending = new Map(); let closing = false; let ownsSocket = false;
  const writeNative = message => output.write(nativeFrame(message));
  const writeClient = (socket, message) => { if (!socket.destroyed) socket.write(`${JSON.stringify(message)}\n`); };
  const disconnect = (socket) => {
    if (!clients.delete(socket)) return;
    if (socket.sessionId) writeNative({ id: randomUUID(), session_id: socket.sessionId, ...(socket.taskKey ? { task_key: socket.taskKey } : {}), method: 'disconnect', params: {} });
    for (const [id, item] of pending) if (item.socket === socket) { clearTimeout(item.timer); pending.delete(id); }
  };
  const server = net.createServer(socket => {
    clients.add(socket); socket.setEncoding('utf8');
    const authTimer = setTimeout(() => socket.destroy(), 3000);
    const decoder = new LineDecoder(message => {
      const { id, session_id, method, params = {} } = message ?? {};
      if (!Number.isSafeInteger(id) || typeof session_id !== 'string' || !/^[\da-f-]{36}$/.test(session_id) || !params || typeof params !== 'object' || Array.isArray(params)) { socket.destroy(); return; }
      if (!socket.sessionId) {
        if (!socket.challenge) {
          if (method !== 'hello' || !isAuthValue(params.client_nonce) || Object.keys(params).some(key => key !== 'client_nonce')) { socket.destroy(); return; }
          socket.challenge = { sessionId: session_id, clientNonce: params.client_nonce, serverNonce: randomBytes(32).toString('hex') };
          writeClient(socket, { id, result: { protocol_version: AUTH_VERSION, server_nonce: socket.challenge.serverNonce, proof: authProof(config.token, 'server', socket.challenge) } });
          return;
        }
        const challenge = socket.challenge;
        const context = { ...challenge, taskKey: params.task_key };
        if (method !== 'authenticate' || session_id !== challenge.sessionId || params.client_nonce !== challenge.clientNonce || params.server_nonce !== challenge.serverNonce || (params.task_key !== undefined && !isAuthValue(params.task_key)) || Object.keys(params).some(key => !['client_nonce', 'server_nonce', 'task_key', 'proof'].includes(key)) || !verifyAuthProof(params.proof, config.token, 'client', context)) { socket.destroy(); return; }
        if ([...clients].some(other => other !== socket && other.sessionId === session_id)) { socket.destroy(); return; }
        socket.sessionId = session_id; socket.taskKey = params.task_key; socket.challenge = undefined;
        clearTimeout(authTimer);
        writeClient(socket, { id, result: { connected: true, proof: authProof(config.token, 'accepted', context) } });
        return;
      }
      if (session_id !== socket.sessionId) { socket.destroy(); return; }
      try { validate(method, params); } catch (error) { writeClient(socket, { id, error: error.message }); return; }
      if (pending.size >= 64 || [...pending.values()].some(item => item.socket === socket)) { writeClient(socket, { id, error: 'A Chrome operation is already pending.' }); return; }
      const routedId = randomUUID();
      const timer = setTimeout(() => { pending.delete(routedId); writeClient(socket, { id, error: 'Chrome response timed out; action effect may be uncertain.' }); socket.destroy(); }, 22000);
      pending.set(routedId, { socket, id, timer });
      writeNative({ id: routedId, session_id, ...(socket.taskKey ? { task_key: socket.taskKey } : {}), method, params });
    });
    socket.on('data', chunk => { try { decoder.push(chunk); } catch { socket.destroy(); } });
    socket.on('error', () => {});
    socket.on('close', () => { clearTimeout(authTimer); disconnect(socket); });
  });
  const decoder = new NativeDecoder(message => {
    const item = pending.get(message?.id); if (!item) return;
    pending.delete(message.id); clearTimeout(item.timer);
    writeClient(item.socket, { id: item.id, ...(typeof message.error === 'string' ? { error: message.error.slice(0, 4000) } : { result: message.result }) });
  });
  input.on('data', chunk => { try { decoder.push(chunk); } catch { void close(); } });
  input.once('end', () => { void close(); }); input.once('error', () => { void close(); });
  output.once('error', () => { void close(); });
  async function close() {
    if (closing) return; closing = true;
    for (const socket of clients) socket.destroy();
    for (const item of pending.values()) clearTimeout(item.timer); pending.clear();
    await new Promise(resolve => server.close(resolve));
    if (ownsSocket && process.platform !== 'win32') await unlink(config.socketPath).catch(() => {});
    input.destroy();
  }
  // Never remove a live connection from a second Chrome profile. A stale socket
  // may be removed only after ECONNREFUSED; every other error fails closed.
  if (process.platform !== 'win32') try {
    const stat = await lstat(config.socketPath);
    if (!stat.isSocket() || stat.uid !== process.getuid()) throw new Error('Unexpected socket path.');
    await new Promise((resolve, reject) => {
      const probe = net.createConnection(config.socketPath);
      probe.once('connect', () => { probe.destroy(); reject(new Error('Another Chrome profile is already connected.')); });
      probe.once('error', error => error.code === 'ECONNREFUSED' ? resolve() : reject(error));
    });
    await unlink(config.socketPath);
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  // libuv creates the first Windows pipe instance exclusively: a second host
  // receives EADDRINUSE and cannot replace a connected Chrome profile.
  try {
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.socketPath, resolve); });
  } catch (error) {
    if (error.code === 'EADDRINUSE') throw new Error('Another Chrome profile is already connected.');
    throw error;
  }
  ownsSocket = true;
  if (process.platform !== 'win32') await chmod(config.socketPath, 0o600);
  writeNative({ event: 'connected', protocol_version: 1 });
  return { close, server };
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const host = await startNativeHost();
    for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void host.close(); });
  } catch { process.stderr.write('Open Harness Chrome host could not connect. Prepare the connection in Open Harness and retry.\n'); process.exitCode = 1; }
}
