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
import { NativeDecoder, nativeFrame } from '../framing.mjs';
import { EXTENSION_ID } from '../constants.mjs';
import { readConfig } from '../config.mjs';
async function fixture(t) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-test-')); await chmod(dir, 0o700);
  const config = { version: 1, socketPath: path.join(dir, 'bridge.sock'), token: randomBytes(32).toString('hex'), extensionId: EXTENSION_ID };
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
  assert.equal((await stat(config.socketPath)).mode & 0o777, 0o600);
  assert.deepEqual(await bridge.request('chrome_status'), { connected: true, shared_tabs: 0, allow_new_tabs: true });
  const routed = messages.find(message => message.method === 'chrome_status');
  assert.match(routed.id, /^[a-f\d-]{36}$/); assert.equal(routed.session_id, bridge.sessionId);
  assert.equal(JSON.stringify(messages).includes(config.token), false);
  bridge.close();
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.ok(messages.some(message => message.method === 'disconnect' && message.session_id === bridge.sessionId));
});
test('bad token and wrong extension origin cannot reach Chrome', async t => {
  const { config, messages } = await fixture(t);
  const socket = net.createConnection(config.socketPath); socket.setEncoding('utf8');
  await new Promise(resolve => socket.once('connect', resolve));
  socket.write(`${JSON.stringify({ id: 1, session_id: '11111111-1111-4111-8111-111111111111', method: 'authenticate', params: { token: '0'.repeat(64) } })}\n`);
  await new Promise(resolve => socket.once('close', resolve));
  assert.equal(messages.some(message => message.method === 'authenticate'), false);
  await assert.rejects(startNativeHost({ config, input: new PassThrough(), output: new PassThrough(), origin: 'chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' }), /Unexpected extension/);
});
test('a second profile cannot steal or unlink the active socket', async t => {
  const { config, bridge } = await fixture(t);
  await assert.rejects(startNativeHost({ config, input: new PassThrough(), output: new PassThrough(), origin: `chrome-extension://${EXTENSION_ID}/` }), /already connected/);
  assert.equal((await bridge.request('chrome_status')).connected, true);
});
test('unsafe configuration permissions fail closed', async t => {
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
test('a persisted private configuration recreates a cleaned temporary socket directory safely', async t => {
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
