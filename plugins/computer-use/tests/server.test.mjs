import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { ComputerBridge } from '../bridge.mjs';
import { tools, validate } from '../tools.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function fixture(t, options) {
  const directory = mkdtempSync(path.join(tmpdir(), 'openharness-computer-test-'));
  const log = path.join(directory, 'calls');
  const bridge = new ComputerBridge(process.execPath, [path.join(root, 'tests/fake-helper.mjs'), log], options);
  t.after(() => { bridge.stop(); rmSync(directory, { recursive: true, force: true }); });
  return { bridge, calls: () => readFileSync(log, 'utf8').trim().split('\n') };
}

test('tool arguments reject ambiguous targets and invalid input before native execution', () => {
  assert.equal(tools.length, 13);
  assert.ok(tools.every(tool => !tool.name.includes('request_accessibility') && !tool.name.includes('request_screen_recording')));
  assert.throws(() => validate('computer_request_accessibility', {}));
  assert.throws(() => validate('request_screen_recording', {}));
  assert.throws(() => validate('computer_select_app', { bundle_id: 'test.app' }));
  assert.throws(() => validate('computer_select_app', { bundle_id: 'test.app', pid: -1 }));
  assert.throws(() => validate('computer_click', { snapshot_id: 'a', ref: 'e1', x: 0, y: 0 }));
  assert.throws(() => validate('computer_click', { snapshot_id: 'a', x: 0 }));
  assert.throws(() => validate('computer_click', { snapshot_id: 'a', x: NaN, y: 0 }));
  assert.throws(() => validate('computer_type', { snapshot_id: 'a', text: 'x'.repeat(2001) }));
  assert.throws(() => validate('computer_key', { snapshot_id: 'a', key: 'cmd+tab' }));
  assert.throws(() => validate('computer_observe', { screenshot: 'true' }));
  assert.throws(() => validate('computer_status', { command: 'anything' }));
  validate('computer_click', { snapshot_id: 'a', ref: 'e1' });
  validate('computer_click', { snapshot_id: 'a', x: 0, y: 0 });
});

test('persistent helper serializes overlapping operations and retains selection', async t => {
  const { bridge, calls } = fixture(t);
  const requests = [bridge.request('select_app', {}), bridge.request('slow', { delay: 30 }), bridge.request('observe', {})];
  const results = await Promise.all(requests);
  assert.deepEqual(calls(), ['select_app', 'slow', 'observe']);
  assert.ok(results.every(result => result.selected && result.pid === results[0].pid));
});

test('helper UTF-8 characters remain intact across pipe chunk boundaries', async t => {
  const { bridge } = fixture(t);
  assert.equal((await bridge.request('unicode_chunks', {})).text, '中文🙂');
});

test('correlated application errors preserve the session without replaying', async t => {
  const { bridge, calls } = fixture(t);
  const before = await bridge.request('select_app', {});
  await assert.rejects(bridge.request('fail', {}), error => {
    assert.match(error.message, /STATE_CHANGED/);
    assert.deepEqual(error.computer, { code: 'STATE_CHANGED', message: 'Observe again.', recoverable: true, dispatched: false, next: 'Call computer_observe.' });
    return true;
  });
  const after = await bridge.request('observe', {});
  assert.equal(before.pid, after.pid);
  assert.deepEqual(calls(), ['select_app', 'fail', 'observe']);
});

test('timeout kills helper, rejects queued work and requires fresh selection', async t => {
  const { bridge, calls } = fixture(t, { timeoutMs: 150 });
  await bridge.request('select_app', {});
  const outcomes = await Promise.allSettled([bridge.request('hang', {}), bridge.request('type', {})]);
  assert.ok(outcomes.every(outcome => outcome.status === 'rejected'));
  assert.match(outcomes[0].reason.message, /timed out/);
  const fresh = await bridge.request('observe', {});
  assert.equal(fresh.selected, false);
  assert.deepEqual(calls(), ['select_app', 'hang', 'observe']);
});

test('cancellation rejects active and queued work without replay', async t => {
  const { bridge, calls } = fixture(t);
  await bridge.request('select_app', {});
  const controller = new AbortController();
  const active = bridge.request('hang', {}, controller.signal);
  const queued = bridge.request('type', {});
  setTimeout(() => controller.abort(), 40);
  const outcomes = await Promise.allSettled([active, queued]);
  assert.ok(outcomes.every(outcome => outcome.status === 'rejected'));
  assert.match(outcomes[0].reason.message, /cancelled/);
  await bridge.request('observe', {});
  assert.deepEqual(calls(), ['select_app', 'hang', 'observe']);
});

test('release rejects work and clears selection', async t => {
  const { bridge } = fixture(t);
  await bridge.request('select_app', {});
  bridge.stop();
  assert.equal((await bridge.request('observe', {})).selected, false);
});

test('Windows shutdown first delivers EOF so the native helper can release input', async t => {
  const { bridge, calls } = fixture(t, { platform: 'win32' });
  await bridge.request('mark_eof', {});
  const exited = once(bridge.child, 'exit');
  bridge.stop();
  assert.deepEqual(await exited, [0, null]);
  assert.deepEqual(calls(), ['mark_eof', 'graceful_eof']);
});

for (const method of ['bad_json', 'wrong_id', 'crash']) test(`${method} invalidates helper and is never replayed`, async t => {
  const { bridge, calls } = fixture(t);
  await assert.rejects(bridge.request(method, {}));
  assert.equal((await bridge.request('observe', {})).selected, false);
  assert.deepEqual(calls(), [method, 'observe']);
});

test('packaged MCP server negotiates and checks native permissions without prompting', { skip: process.platform !== 'darwin' }, async t => {
  const child = spawn(process.execPath, [path.join(root, 'server.mjs')], {
    stdio: ['pipe', 'pipe', 'pipe'],
    // Protocol/unit tests deliberately use stdio and never launch a GUI app.
    env: { ...process.env, OPENHARNESS_COMPUTER_APP: '', OPENHARNESS_COMPUTER_HELPER: process.env.OPENHARNESS_COMPUTER_TEST_HELPER || path.join(root, 'bin', 'computer-helper') },
  });
  t.after(() => { child.stdin.end(); child.kill(); });
  child.stderr.resume();
  const lines = createInterface({ input: child.stdout });
  let sequence = 0; const pending = new Map();
  lines.on('line', line => { const result = JSON.parse(line); pending.get(result.id)?.(result); pending.delete(result.id); });
  function request(method, params) {
    return new Promise((resolve, reject) => {
      const id = ++sequence;
      const timer = setTimeout(() => reject(new Error(`MCP ${method} timeout`)), 10_000);
      pending.set(id, result => { clearTimeout(timer); resolve(result); });
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  const invalidInit = await request('initialize', null);
  assert.equal(invalidInit.error.code, -32602);
  const init = await request('initialize', { protocolVersion: '2025-11-25' });
  assert.equal(init.result.serverInfo.name, 'openharness-computer-use');
  const forbiddenMethod = await request('request_accessibility', {});
  assert.equal(forbiddenMethod.error.code, -32601);
  const forbiddenTool = await request('tools/call', { name: 'computer_request_screen_recording' });
  assert.equal(forbiddenTool.result.isError, true);
  assert.match(forbiddenTool.result.content[0].text, /Unknown computer tool/);
  const catalog = await request('tools/list', {});
  assert.equal(catalog.result.tools.length, 13);
  const status = await request('tools/call', { name: 'computer_status' });
  assert.equal(status.result.isError, undefined, status.result.content[0].text);
  const state = JSON.parse(status.result.content[0].text);
  assert.equal(state.permissions_requested, false);
  assert.equal(typeof state.accessibility, 'boolean');
  assert.equal(typeof state.screen_recording, 'boolean');
  assert.ok(path.isAbsolute(state.helper_path));
  const selection = await request('tools/call', { name: 'computer_select_app', arguments: { bundle_id: 'dev.openharness.nonexistent-fixture', pid: 2147483647 } });
  assert.equal(selection.result.isError, true);
  assert.match(selection.result.content[0].text, state.accessibility ? /APP_NOT_FOUND/ : /ACCESSIBILITY_PERMISSION_REQUIRED/);
  const observe = await request('tools/call', { name: 'computer_observe' });
  assert.equal(observe.result.isError, true);
  assert.match(observe.result.content[0].text, /APP_NOT_SELECTED/);
});
