import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { PassThrough } from 'node:stream';
import { mkdtemp, chmod, writeFile, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { randomBytes } from 'node:crypto';
import { ChromeBridge } from '../bridge.mjs';
import { startNativeHost } from '../native-host.mjs';
import { LineDecoder, NativeDecoder, nativeFrame } from '../framing.mjs';
import { EXTENSION_ID } from '../constants.mjs';
import { readConfig } from '../config.mjs';
import { validateSocketPath } from '../endpoint.mjs';
import { AUTH_VERSION, authProof, verifyAuthProof } from '../auth.mjs';

async function rawClient(t, config, sessionId = '33333333-3333-4333-8333-333333333333') {
  const socket = net.createConnection(config.socketPath); socket.setEncoding('utf8');
  t.after(() => socket.destroy());
  await new Promise((resolve, reject) => { socket.once('connect', resolve); socket.once('error', reject); });
  let id = 0;
  const response = () => new Promise(resolve => socket.once('data', data => resolve(JSON.parse(data))));
  const send = (method, params, extra = {}) => socket.write(`${JSON.stringify({ id: ++id, session_id: sessionId, method, params, ...extra })}\n`);
  const request = async (method, params, extra) => { const reply = response(); send(method, params, extra); return reply; };
  const hello = async (clientNonce = randomBytes(32).toString('hex')) => {
    const result = (await request('hello', { client_nonce: clientNonce })).result;
    const context = { sessionId, clientNonce, serverNonce: result.server_nonce };
    assert.equal(result.protocol_version, AUTH_VERSION);
    assert.ok(verifyAuthProof(result.proof, config.token, 'server', context));
    return context;
  };
  const authenticate = async (taskKey) => {
    const context = { ...await hello(), taskKey };
    const result = (await request('authenticate', authentication(config.token, context))).result;
    assert.equal(result.connected, true);
    assert.ok(verifyAuthProof(result.proof, config.token, 'accepted', context));
  };
  const rejected = async (params, extra) => {
    const closed = new Promise(resolve => socket.once('close', resolve));
    send('authenticate', params, extra); await closed;
  };
  return { socket, request, hello, authenticate, rejected };
}

function authentication(token, context) {
  return { client_nonce: context.clientNonce, server_nonce: context.serverNonce, ...(context.taskKey === undefined ? {} : { task_key: context.taskKey }), proof: authProof(token, 'client', context) };
}
async function fixture(t) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-test-')); await chmod(dir, 0o700);
  const socketPath = process.platform === 'win32' ? `\\\\.\\pipe\\openharness-chrome-${randomBytes(16).toString('hex')}` : path.join(dir, 'bridge.sock');
  const config = { version: 1, socketPath, token: randomBytes(32).toString('hex'), extensionId: EXTENSION_ID };
  const configPath = path.join(dir, 'config.json'); await writeFile(configPath, JSON.stringify(config), { mode: 0o600 });
  const input = new PassThrough(); const output = new PassThrough(); const messages = [];
  const decoder = new NativeDecoder(message => {
    messages.push(message);
    if (message.method === 'chrome_status') input.write(nativeFrame({ id: message.id, result: { connected: true, shared_tabs: 0, allow_new_tabs: true } }));
  }); output.on('data', data => decoder.push(data));
  const host = await startNativeHost({ config, input, output, origin: `chrome-extension://${EXTENSION_ID}/` });
  const bridge = new ChromeBridge({ configPath, timeoutMs: 500 });
  t.after(async () => { bridge.close(); await host.close(); await rm(dir, { recursive: true, force: true }); });
  return { dir, configPath, config, host, bridge, messages, input, output };
}
test('real local socket authenticates, routes status, and releases a disconnected session', async t => {
  const { bridge, messages, config } = await fixture(t);
  if (process.platform !== 'win32') assert.equal((await stat(config.socketPath)).mode & 0o777, 0o600);
  assert.deepEqual(await bridge.request('chrome_status'), { connected: true, shared_tabs: 0, allow_new_tabs: true });
  const routed = messages.find(message => message.method === 'chrome_status');
  assert.match(routed.id, /^[a-f\d-]{36}$/); assert.equal(routed.session_id, bridge.sessionId);
  assert.equal(JSON.stringify(messages).includes(config.token), false);
  bridge.close();
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.ok(messages.some(message => message.method === 'disconnect' && message.session_id === bridge.sessionId));
});
test('legacy plaintext authentication and wrong extension origin cannot reach Chrome', async t => {
  const { config, messages } = await fixture(t);
  const socket = net.createConnection(config.socketPath); socket.setEncoding('utf8');
  await new Promise(resolve => socket.once('connect', resolve));
  socket.write(`${JSON.stringify({ id: 1, session_id: '11111111-1111-4111-8111-111111111111', method: 'authenticate', params: { token: config.token } })}\n`);
  await new Promise(resolve => socket.once('close', resolve));
  assert.equal(messages.some(message => message.method === 'authenticate'), false);
  await assert.rejects(startNativeHost({ config, input: new PassThrough(), output: new PassThrough(), origin: 'chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' }), /Unexpected extension/);
});
test('task layout keys survive bridge replacement while transport sessions remain distinct', async t => {
  const { configPath, config, messages } = await fixture(t);
  const first = new ChromeBridge({ configPath, taskId: 'run-a' });
  const replacement = new ChromeBridge({ configPath, taskId: 'run-a' });
  const other = new ChromeBridge({ configPath, taskId: 'run-b' });
  t.after(() => { first.close(); replacement.close(); other.close(); });
  await first.request('chrome_status');
  await replacement.request('chrome_status');
  await other.request('chrome_status');
  const routed = messages.filter(message => message.method === 'chrome_status');
  assert.equal(routed.length, 3);
  assert.match(routed[0].task_key, /^[a-f0-9]{64}$/);
  assert.equal(routed[0].task_key, routed[1].task_key);
  assert.notEqual(routed[0].task_key, routed[2].task_key);
  assert.notEqual(routed[0].session_id, routed[1].session_id);
  assert.equal(JSON.stringify(messages).includes(config.token), false);
  assert.equal(JSON.stringify(messages).includes('run-a'), false);
  first.close();
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.ok(messages.some(message => message.method === 'disconnect' && message.session_id === first.sessionId && message.task_key === routed[0].task_key));
});
test('task metadata is bound at authentication and cannot be supplied by a model tool', async t => {
  const { config, messages } = await fixture(t);
  const client = await rawClient(t, config);
  await client.authenticate('a'.repeat(64));
  assert.equal((await client.request('chrome_status', {}, { task_key: 'b'.repeat(64) })).result.connected, true);
  assert.equal(messages.find(message => message.method === 'chrome_status').task_key, 'a'.repeat(64));
  assert.match((await client.request('chrome_status', { task_key: 'b'.repeat(64) })).error, /Unknown argument/);
  assert.equal(messages.filter(message => message.method === 'chrome_status').length, 1);
});
test('malformed task identity fails before forwarding any Chrome request', async t => {
  const { config, messages } = await fixture(t);
  const client = await rawClient(t, config);
  const context = { ...await client.hello(), taskKey: '../private' };
  await client.rejected(authentication(config.token, context));
  assert.equal(messages.some(message => message.method), false);
  for (const taskId of ['', 'x'.repeat(257), 'bad\0id', 42]) assert.throws(() => new ChromeBridge({ taskId }), /Invalid Chrome task identity/);
});

test('an impersonated host receives no credential, client proof, task key or Chrome operation', async t => {
  const { config, configPath, host } = await fixture(t);
  await host.close();
  const received = [];
  const sockets = new Set();
  const server = net.createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket));
    const decoder = new LineDecoder(message => {
      received.push(message);
      socket.write(`${JSON.stringify({ id: message.id, result: { protocol_version: AUTH_VERSION, server_nonce: 'f'.repeat(64), proof: '0'.repeat(64) } })}\n`);
    });
    socket.setEncoding('utf8'); socket.on('data', data => decoder.push(data));
  });
  t.after(async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)); });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.socketPath, resolve); });
  const bridge = new ChromeBridge({ configPath, taskId: 'private-task-title', timeoutMs: 500 });
  t.after(() => bridge.close());
  await assert.rejects(bridge.request('chrome_status'), /identity could not be verified/);
  assert.equal(received.length, 1);
  assert.equal(received[0].method, 'hello');
  assert.deepEqual(Object.keys(received[0].params), ['client_nonce']);
  assert.match(received[0].params.client_nonce, /^[a-f0-9]{64}$/);
  assert.equal(JSON.stringify(received).includes(config.token), false);
  assert.equal(JSON.stringify(received).includes('private-task-title'), false);
  assert.equal(bridge.socket, undefined);
});

test('a different configured secret cannot authenticate the native host', async t => {
  const { config, configPath, messages } = await fixture(t);
  await writeFile(configPath, JSON.stringify({ ...config, token: '0'.repeat(64) }), { mode: 0o600 });
  const bridge = new ChromeBridge({ configPath, timeoutMs: 500 }); t.after(() => bridge.close());
  await assert.rejects(bridge.request('chrome_status'), /identity could not be verified/);
  assert.equal(messages.some(message => message.method), false);
});

test('captured server proofs, changed nonces, old protocol and invalid confirmation never dispatch an operation', async t => {
  const { config, configPath, host } = await fixture(t);
  await host.close();
  let mode; const sockets = new Set(); const received = [];
  const server = net.createServer(socket => {
    const testMode = mode; let context;
    sockets.add(socket); socket.on('close', () => sockets.delete(socket));
    const decoder = new LineDecoder(message => {
      received.push(message.method);
      if (message.method === 'hello') {
        context = { sessionId: message.session_id, clientNonce: message.params.client_nonce, serverNonce: 'b'.repeat(64) };
        const signed = { ...context };
        if (testMode === 'captured') signed.clientNonce = 'c'.repeat(64);
        if (testMode === 'changed nonce') signed.serverNonce = 'd'.repeat(64);
        socket.write(`${JSON.stringify({ id: message.id, result: { protocol_version: testMode === 'old protocol' ? 1 : AUTH_VERSION, server_nonce: context.serverNonce, proof: authProof(config.token, 'server', signed) } })}\n`);
      } else {
        assert.equal(message.method, 'authenticate');
        // The confirmation is valid for another task, not this authenticated session.
        socket.write(`${JSON.stringify({ id: message.id, result: { connected: true, proof: authProof(config.token, 'accepted', { ...context, taskKey: 'e'.repeat(64) }) } })}\n`);
      }
    });
    socket.setEncoding('utf8'); socket.on('data', data => decoder.push(data));
  });
  t.after(async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)); });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.socketPath, resolve); });
  for (mode of ['captured', 'changed nonce', 'old protocol', 'confirmation']) {
    received.length = 0;
    const bridge = new ChromeBridge({ configPath, taskId: 'test-task', timeoutMs: 500 });
    t.after(() => bridge.close());
    await assert.rejects(bridge.request('chrome_status'), mode === 'confirmation' ? /not confirmed/ : /identity could not be verified/);
    assert.deepEqual(received, mode === 'confirmation' ? ['hello', 'authenticate'] : ['hello']);
    assert.equal(bridge.socket, undefined);
  }
});

test('client authentication binds both nonces, session and task key and separates proof roles', async t => {
  const { config, messages } = await fixture(t);
  for (const change of ['secret', 'client nonce', 'server nonce', 'session', 'task', 'role']) {
    const client = await rawClient(t, config);
    const context = { ...await client.hello(), taskKey: 'a'.repeat(64) };
    const params = authentication(change === 'secret' ? '0'.repeat(64) : config.token, context);
    if (change === 'client nonce') params.client_nonce = 'c'.repeat(64);
    if (change === 'server nonce') params.server_nonce = 'd'.repeat(64);
    if (change === 'task') params.task_key = 'b'.repeat(64);
    if (change === 'role') params.proof = authProof(config.token, 'server', context);
    await client.rejected(params, change === 'session' ? { session_id: '44444444-4444-4444-8444-444444444444' } : {});
  }
  assert.equal(messages.some(message => message.method), false);
});

test('a captured client proof cannot be replayed on a replacement connection', async t => {
  const { config, messages } = await fixture(t);
  const first = await rawClient(t, config);
  const context = { ...await first.hello('a'.repeat(64)), taskKey: 'b'.repeat(64) };
  const captured = authentication(config.token, context);
  const closed = new Promise(resolve => first.socket.once('close', resolve)); first.socket.destroy(); await closed;
  const replacement = await rawClient(t, config);
  const fresh = await replacement.hello(context.clientNonce);
  assert.notEqual(fresh.serverNonce, context.serverNonce);
  await replacement.rejected(captured);
  assert.equal(messages.some(message => message.method), false);
});
test('a second profile cannot steal or unlink the active socket', async t => {
  const { config, bridge } = await fixture(t);
  await assert.rejects(startNativeHost({ config, input: new PassThrough(), output: new PassThrough(), origin: `chrome-extension://${EXTENSION_ID}/` }), /already connected/);
  assert.equal((await bridge.request('chrome_status')).connected, true);
});
test('unsafe configuration permissions fail closed', { skip: process.platform === 'win32' }, async t => {
  const { configPath } = await fixture(t);
  await chmod(configPath, 0o644); await assert.rejects(readConfig(configPath), /permissions/);
});
test('native framing handles split messages and rejects oversize frames', () => {
  const messages = []; const decoder = new NativeDecoder(message => messages.push(message));
  const frame = nativeFrame({ value: '中文' });
  for (const byte of frame) decoder.push(Buffer.from([byte]));
  assert.deepEqual(messages, [{ value: '中文' }]);
  const bad = Buffer.alloc(4); bad.writeUInt32LE(10 * 1024 * 1024);
  assert.throws(() => decoder.push(bad), /Invalid native message size/);
});
test('transport timeout never retries a pending action and detaches the session', async t => {
  const { bridge, messages } = await fixture(t);
  await assert.rejects(bridge.request('chrome_open', { url: 'https://example.com' }), /timed out/);
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(messages.filter(message => message.method === 'chrome_open').length, 1);
  assert.ok(messages.some(message => message.method === 'disconnect'));
});
test('a persisted private configuration recreates a cleaned temporary socket directory safely', { skip: process.platform === 'win32' }, async t => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'oh-config-test-')); await chmod(dir, 0o700);
  t.after(() => rm(dir, { recursive: true, force: true }));
  const configPath = path.join(dir, 'config.json'); const socketDirectory = path.join(dir, 'after-reboot');
  const config = { version: 1, socketPath: path.join(socketDirectory, 'b.sock'), token: randomBytes(32).toString('hex'), extensionId: EXTENSION_ID };
  await writeFile(configPath, JSON.stringify(config), { mode: 0o600 });
  assert.equal((await readConfig(configPath)).socketPath, config.socketPath);
  assert.equal((await stat(socketDirectory)).mode & 0o777, 0o700);
  await chmod(socketDirectory, 0o755);
  await assert.rejects(readConfig(configPath), /socket directory must be private/);
  assert.equal((await stat(socketDirectory)).mode & 0o777, 0o755); // Never silently weaken/repair a hostile existing directory.
});
test('Windows accepts only this application local named pipe namespace', () => {
  assert.doesNotThrow(() => validateSocketPath(`\\\\.\\pipe\\openharness-chrome-${'a'.repeat(16)}`, 'win32'));
  for (const value of [
    'C:\\bridge.sock', '/tmp/bridge.sock', 'tcp://localhost:1234',
    `\\\\remote\\pipe\\openharness-chrome-${'a'.repeat(16)}`,
    `\\\\.\\pipe\\other-app-${'a'.repeat(16)}`,
    `\\\\.\\pipe\\openharness-chrome-${'a'.repeat(16)}\\..\\other`,
    `\\\\.\\pipe\\openharness-chrome-${'a'.repeat(16)}\0`,
    `\\\\.\\pipe\\openharness-chrome-short`,
  ]) assert.throws(() => validateSocketPath(value, 'win32'), /Invalid Chrome bridge/);
});
test('closing the native host releases the endpoint for the next Chrome connection', async t => {
  const { config, configPath, bridge, host } = await fixture(t);
  assert.equal((await bridge.request('chrome_status')).connected, true);
  bridge.close(); await host.close();
  const input = new PassThrough(); const output = new PassThrough();
  const decoder = new NativeDecoder(message => {
    if (message.method === 'chrome_status') input.write(nativeFrame({ id: message.id, result: { connected: true } }));
  }); output.on('data', chunk => decoder.push(chunk));
  const nextHost = await startNativeHost({ config, input, output, origin: `chrome-extension://${EXTENSION_ID}/` });
  const nextBridge = new ChromeBridge({ configPath });
  t.after(async () => { nextBridge.close(); await nextHost.close(); });
  assert.equal((await nextBridge.request('chrome_status')).connected, true);
});
