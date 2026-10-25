import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeConnection, installBackground } from '../extension/background.mjs';
import { ChromeController } from '../extension/controller.mjs';

const flush = () => new Promise(resolve => setImmediate(resolve));
const event = () => {
  const listeners = [];
  return { addListener(listener) { listeners.push(listener); }, emit(...args) { return listeners.map(listener => listener(...args)); } };
};
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

function fixture({ stored = {}, alarms = new Map(), time = 100_000, controller, alarmsAvailable = true } = {}) {
  let now = time; let timerId = 0;
  const timers = new Map(); const ports = []; const detached = [];
  const clock = {
    now: () => now,
    schedule(callback, delay) { const id = ++timerId; timers.set(id, { callback, at: now + delay }); return id; },
    cancel(id) { timers.delete(id); },
  };
  const api = {
    runtime: {
      id: 'extension-id', getURL: path => `chrome-extension://extension-id/${path}`,
      onStartup: event(), onInstalled: event(), onMessage: event(),
      connectNative(name) {
        assert.equal(name, 'dev.openharness.chrome');
        const port = {
          onMessage: event(), onDisconnect: event(), messages: [], disconnected: false,
          postMessage(message) { if (this.disconnected) throw new Error('Port closed'); this.messages.push(message); },
          // Chrome does not emit onDisconnect at the end that calls disconnect.
          disconnect() { this.disconnected = true; },
          crash() { this.disconnected = true; api.runtime.lastError = { message: 'Native host has exited.' }; this.onDisconnect.emit(); delete api.runtime.lastError; },
          acknowledge() { this.onMessage.emit({ event: 'connected', protocol_version: 1 }); },
        };
        ports.push(port); return port;
      },
    },
    storage: { local: {
      async get() { return structuredClone(stored); },
      async set(value) { Object.assign(stored, structuredClone(value)); },
    } },
    alarms: {
      onAlarm: event(),
      async create(name, value) { alarms.set(name, value); },
      async clear(name) { return alarms.delete(name); },
    },
    tabs: { onRemoved: event(), onUpdated: event(), async query() { return []; } },
    debugger: { onDetach: event(), async detach({ tabId }) { detached.push(tabId); } },
  };
  if (!alarmsAvailable) delete api.alarms;
  controller ??= new ChromeController(api);
  const connection = new NativeConnection(api, controller, clock);
  async function advance(milliseconds, { timersEnabled = true } = {}) {
    const end = now + milliseconds;
    if (timersEnabled) while (true) {
      const next = [...timers.entries()].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (!next) break;
      timers.delete(next[0]); now = next[1].at; next[1].callback(); await flush();
    }
    now = end;
  }
  async function wakeAlarm() {
    const [name, alarm] = [...alarms][0] ?? [];
    assert.ok(name, 'a wakeup alarm exists');
    now = Math.max(now, alarm.when); alarms.delete(name); api.alarms.onAlarm.emit({ name }); await flush();
  }
  return { api, connection, controller, ports, stored, alarms, timers, detached, advance, wakeAlarm, now: () => now };
}

test('existing installations still connect automatically and restore only the background-tab preference', async () => {
  const f = fixture({ stored: { allowNewTabs: true } });
  await f.connection.resume();
  assert.equal(f.ports.length, 1); assert.equal(f.connection.status().connecting, true);
  f.ports[0].acknowledge(); await flush();
  assert.equal(f.connection.status().connected, true);
  assert.equal(f.controller.allowNewTabs, true);
  assert.equal(f.controller.shared.size, 0);
  assert.equal(f.alarms.size, 0); assert.equal(f.timers.size, 0);
});

test('scripts upgraded before their manifest can connect and retry without the new alarms permission', async () => {
  const f = fixture({ alarmsAvailable: false });
  await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  assert.equal(f.connection.status().connected, true); assert.equal(f.connection.status().reloadRequired, true);
  f.ports[0].crash(); await flush(); await f.advance(1000);
  assert.equal(f.ports.length, 2); f.ports[1].acknowledge(); await flush();
  assert.equal(f.connection.status().connected, true);
  await f.connection.disconnect();
  assert.equal(f.stored.connectionEnabled, false); assert.equal(f.timers.size, 0);
});

test('a terminated native host reconnects after cleanup without retaining tab grants', async () => {
  const f = fixture({ stored: { allowNewTabs: true } });
  await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  f.controller.shared.set(4, 'https://example.com');
  f.controller.owned.set(5, { sessionId: 'old-session', origin: 'https://example.com' });
  f.controller.sessions.set('old-session', { tabId: 5 }); f.controller.attached.set(5, 'old-session');
  f.ports[0].crash(); await flush();
  assert.deepEqual(f.detached, [5]);
  assert.equal(f.controller.shared.size, 0); assert.equal(f.controller.owned.size, 0);
  assert.equal(f.controller.allowNewTabs, true);
  assert.equal(f.connection.status().retrying, true);
  await f.advance(999); assert.equal(f.ports.length, 1);
  await f.advance(1); assert.equal(f.ports.length, 2);
  f.ports[1].acknowledge(); await flush();
  assert.equal(f.connection.status().connected, true);
  assert.equal(f.alarms.size, 0);
});

test('successive connection failures back off to one minute and a successful handshake resets the delay', async () => {
  const f = fixture(); await f.connection.resume();
  for (const delay of [1000, 2000, 4000, 8000, 16_000, 32_000, 60_000, 60_000]) {
    f.ports.at(-1).crash(); await flush();
    assert.equal(f.stored.connectionRetry.at - f.now(), delay);
    const count = f.ports.length; await f.advance(delay);
    assert.equal(f.ports.length, count + 1);
  }
  f.ports.at(-1).acknowledge(); await flush();
  f.ports.at(-1).crash(); await flush();
  assert.equal(f.stored.connectionRetry.at - f.now(), 1000);
});

test('the wakeup alarm recovers after the worker loses its timer and persisted deadlines prevent fast restart loops', async () => {
  const first = fixture(); await first.connection.resume(); first.ports[0].crash(); await flush();
  const replacement = fixture({ stored: first.stored, alarms: first.alarms, time: first.now() });
  await replacement.connection.resume();
  assert.equal(replacement.ports.length, 0, 'worker restart does not bypass the retry deadline');
  replacement.timers.clear(); // Simulate worker suspension losing timeout handles.
  await replacement.wakeAlarm();
  assert.equal(replacement.ports.length, 1);
  replacement.ports[0].acknowledge(); await flush();
  assert.equal(replacement.connection.status().connected, true);
});

test('explicit disconnect cancels retries and persists across worker/browser startup until the user connects', async () => {
  const f = fixture(); await f.connection.resume(); f.ports[0].crash(); await flush();
  await f.connection.disconnect();
  assert.equal(f.alarms.size, 0); assert.equal(f.timers.size, 0);
  assert.equal(f.stored.connectionEnabled, false);
  await f.advance(120_000); assert.equal(f.ports.length, 1);
  const restarted = fixture({ stored: f.stored, time: f.now() });
  await restarted.connection.resume();
  restarted.api.alarms.onAlarm.emit({ name: 'openharness-native-reconnect' }); await flush();
  assert.equal(restarted.ports.length, 0);
  await restarted.connection.connect();
  assert.equal(restarted.ports.length, 1); assert.equal(f.stored.connectionEnabled, true);
});

test('manual disconnect clears connected state and grants even without an onDisconnect event', async () => {
  const f = fixture(); await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  f.controller.shared.set(4, 'https://example.com');
  await f.connection.disconnect();
  assert.equal(f.ports[0].disconnected, true);
  assert.equal(f.connection.status().connected, false); assert.equal(f.connection.status().enabled, false);
  assert.equal(f.controller.shared.size, 0);
});

test('a storage failure during manual disconnect still releases the port and tab grants', async () => {
  const f = fixture(); await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  f.controller.shared.set(4, 'https://example.com');
  f.api.storage.local.set = async () => { throw new Error('Storage unavailable'); };
  await assert.rejects(f.connection.disconnect(), /Storage unavailable/);
  assert.equal(f.ports[0].disconnected, true); assert.equal(f.connection.status().enabled, false);
  assert.equal(f.controller.shared.size, 0); assert.equal(f.alarms.size, 0);
});

test('reconnect waits for old cleanup, and stale port messages or callbacks cannot affect the new connection', async () => {
  const cleanup = deferred(); let disconnects = 0; let calls = 0;
  const f = fixture({ controller: { async disconnect() { disconnects++; await cleanup.promise; }, async call() { calls++; } } });
  await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  const old = f.ports[0]; old.crash(); await flush();
  const reconnect = f.connection.connect(); await flush();
  assert.equal(f.ports.length, 1);
  cleanup.resolve(); await reconnect;
  assert.equal(f.ports.length, 2); f.ports[1].acknowledge(); await flush();
  old.acknowledge(); old.crash(); old.onMessage.emit({ id: 'old', session_id: 'old', method: 'chrome_open' }); await flush();
  assert.equal(f.connection.status().connected, true);
  assert.equal(disconnects, 1); assert.equal(calls, 0); assert.equal(f.alarms.size, 0);
});

test('an interrupted action is never replayed or delivered to the replacement port', async () => {
  const action = deferred(); let calls = 0;
  const f = fixture({ controller: { async disconnect() { await action.promise; }, async call() { calls++; return action.promise; } } });
  await f.connection.resume(); f.ports[0].acknowledge(); await flush();
  const old = f.ports[0]; old.onMessage.emit({ id: 'click-1', session_id: 'session', method: 'chrome_click', params: {} }); await flush();
  assert.equal(calls, 1); old.crash(); await flush();
  action.resolve({ clicked: true }); await flush(); await f.advance(1000);
  f.ports[1].acknowledge(); await flush();
  assert.equal(calls, 1); assert.deepEqual(old.messages, []); assert.deepEqual(f.ports[1].messages, []);
});

test('a silent host handshake times out and retries rather than remaining connecting forever', async () => {
  const f = fixture(); await f.connection.resume();
  await f.advance(10_000);
  assert.equal(f.ports[0].disconnected, true); assert.equal(f.connection.status().retrying, true);
  await f.advance(1000); assert.equal(f.ports.length, 2);
});

test('synchronous native launch failures also retry and browser startup respects a saved disconnect', async () => {
  const f = fixture(); f.api.runtime.connectNative = () => { throw new Error('Native host not registered'); };
  await f.connection.resume();
  assert.equal(f.connection.status().connecting, false); assert.equal(f.connection.status().retrying, true);
  assert.match(f.connection.status().error, /not registered/);
  const stopped = fixture({ stored: { connectionEnabled: false } });
  const background = installBackground(stopped.api);
  stopped.api.runtime.onStartup.emit(); stopped.api.runtime.onInstalled.emit(); await flush();
  assert.equal(stopped.ports.length, 0); assert.equal(background.status().enabled, false);
  const ignored = stopped.api.runtime.onMessage.emit({ action: 'connect' }, { id: stopped.api.runtime.id, tab: { id: 1 }, url: stopped.api.runtime.getURL('popup.html') }, () => { throw new Error('Should not respond'); });
  assert.deepEqual(ignored, [false]); assert.equal(stopped.ports.length, 0);
});
