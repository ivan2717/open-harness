import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { launchChrome, requestWithLaunch } from '../launch-chrome.mjs';
test('approved open starts Chrome in background with fixed arguments', async () => {
  let actual;
  await launchChrome({ platform: 'darwin', spawnProcess(command, args, options) { actual = { command, args, options }; const child = new EventEmitter(); queueMicrotask(() => child.emit('close', 0)); return child; } });
  assert.deepEqual(actual, { command: '/usr/bin/open', args: ['-g', '-b', 'com.google.Chrome'], options: { stdio: 'ignore' } });
});
test('only connection setup retries; an action failure is never retried', async () => {
  let attempts = 0; let launched = 0; let requests = 0;
  const bridge = { async connect() { if (++attempts < 3) throw new Error('offline'); }, async request() { requests++; throw new Error('uncertain effect'); } };
  await assert.rejects(requestWithLaunch(bridge, 'chrome_open', { url: 'https://example.com' }, { launch: async () => { launched++; }, wait: async () => {}, attempts: 5 }), /uncertain effect/);
  assert.equal(launched, 1); assert.equal(requests, 1); assert.equal(attempts, 3);
});
test('status and reads cannot launch apps', async () => {
  let launched = false;
  const bridge = { async connect() { throw new Error('offline'); } };
  for (const method of ['chrome_status', 'chrome_snapshot', 'chrome_action']) await assert.rejects(requestWithLaunch(bridge, method, {}, { launch: async () => { launched = true; } }), /offline/);
  assert.equal(launched, false);
});
test('cancellation during Chrome startup does not dispatch the requested operation', async () => {
  let cancelled = false; let requests = 0;
  const bridge = { async connect() { throw new Error('offline'); }, async request() { requests++; } };
  await assert.rejects(requestWithLaunch(bridge, 'chrome_open', {}, { launch: async () => {}, wait: async () => { cancelled = true; }, shouldCancel: () => cancelled }), /cancelled/);
  assert.equal(requests, 0);
});
