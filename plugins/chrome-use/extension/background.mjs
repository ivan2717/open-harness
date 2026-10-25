import { ChromeController } from './controller.mjs';

const RECONNECT_ALARM = 'openharness-native-reconnect';
const MAX_RETRY_DELAY = 60_000;
const HANDSHAKE_TIMEOUT = 10_000;

// Reconnect the transport only. In-flight actions and tab grants are deliberately
// discarded: their effects may be uncertain after a native host disconnects.
export class NativeConnection {
  constructor(api, controller, {
    now = Date.now,
    // Browser timer functions require their original Window/Worker receiver.
    // Calling an extracted Web API as this.schedule()/this.cancel() is invalid.
    schedule = (callback, delay) => globalThis.setTimeout(callback, delay),
    cancel = id => globalThis.clearTimeout(id),
  } = {}) {
    this.api = api; this.controller = controller;
    this.now = now; this.schedule = schedule; this.cancel = cancel;
    this.enabled = true; this.connected = false; this.connecting = false; this.error = '';
    this.attempt = 0; this.retryAt = 0; this.lifecycle = Promise.resolve();
    this.ready = api.storage.local.get(['allowNewTabs', 'connectionEnabled', 'connectionRetry']).then(settings => {
      controller.allowNewTabs = settings.allowNewTabs === true;
      this.enabled = settings.connectionEnabled !== false;
      const retry = settings.connectionRetry;
      if (Number.isInteger(retry?.attempt) && retry.attempt >= 0 && Number.isFinite(retry?.at)) {
        this.attempt = Math.min(retry.attempt, 6);
        this.retryAt = Math.min(Math.max(0, retry.at), this.now() + MAX_RETRY_DELAY);
      }
    });
    // During an unpacked upgrade Chrome can see new scripts before it reloads
    // the manifest permission. Keep popup/short retries usable in that window.
    api.alarms?.onAlarm.addListener(alarm => {
      if (alarm.name === RECONNECT_ALARM) this.run(() => this.resume());
    });
  }

  enqueue(operation) {
    const pending = this.lifecycle.then(async () => { await this.ready; return operation(); });
    this.lifecycle = pending.catch(() => {});
    return pending;
  }

  run(operation) { void operation().catch(error => { this.error = String(error.message ?? error).slice(0, 4000); }); }

  status() {
    return { connected: this.connected, connecting: this.connecting, error: this.error, enabled: this.enabled, retrying: this.enabled && this.retryAt > 0, reloadRequired: !this.api.alarms };
  }

  resume() {
    return this.enqueue(async () => {
      if (!this.enabled) { await this.cancelRetry(); return; }
      if (this.port) return;
      if (this.retryAt > this.now()) await this.armRetry();
      else await this.open();
    });
  }

  connect() {
    return this.enqueue(async () => {
      this.enabled = true;
      await this.api.storage.local.set({ connectionEnabled: true });
      if (this.port) return;
      this.attempt = 0; this.retryAt = 0;
      await this.open();
    });
  }

  disconnect() {
    return this.enqueue(async () => {
      this.enabled = false; this.error = ''; this.retryAt = 0; this.attempt = 0;
      const port = this.port;
      this.forgetPort();
      // Disconnecting our end need not emit onDisconnect, so clean up explicitly.
      try { port?.disconnect(); } catch { /* The host may already have exited. */ }
      const cleanup = this.controller.disconnect();
      try {
        await this.cancelRetry();
        await this.api.storage.local.set({ connectionEnabled: false, connectionRetry: { attempt: 0, at: 0 } });
      } finally { await cleanup; }
    });
  }

  forgetPort() {
    this.port = undefined; this.connected = false; this.connecting = false;
    this.cancel(this.handshakeTimer); this.handshakeTimer = undefined;
  }

  async cancelRetry() {
    this.cancel(this.retryTimer); this.retryTimer = undefined;
    await this.api.alarms?.clear(RECONNECT_ALARM);
  }

  async armRetry() {
    this.cancel(this.retryTimer);
    const delay = Math.max(0, this.retryAt - this.now());
    // Short delays recover promptly while the worker is awake. An alarm survives
    // MV3 worker suspension; the persisted deadline prevents restart retry loops.
    await this.api.alarms?.create(RECONNECT_ALARM, { when: this.now() + Math.max(MAX_RETRY_DELAY, delay) });
    this.retryTimer = this.schedule(() => this.run(() => this.resume()), delay);
  }

  async retry() {
    if (!this.enabled || this.port) return;
    const delay = Math.min(MAX_RETRY_DELAY, 1000 * 2 ** this.attempt);
    this.attempt = Math.min(this.attempt + 1, 6); this.retryAt = this.now() + delay;
    await this.api.storage.local.set({ connectionRetry: { attempt: this.attempt, at: this.retryAt } });
    await this.armRetry();
  }

  async open() {
    if (!this.enabled || this.port) return;
    await this.cancelRetry();
    this.retryAt = 0; this.connecting = true; this.error = '';
    try {
      const port = this.api.runtime.connectNative('dev.openharness.chrome');
      this.port = port;
      port.onMessage.addListener(message => { this.run(() => this.receive(port, message)); });
      port.onDisconnect.addListener(() => {
        // Read lastError during this callback, including stale-port callbacks.
        const error = this.api.runtime.lastError?.message ?? 'Connection closed.';
        this.lost(port, error);
      });
      this.handshakeTimer = this.schedule(() => {
        if (this.port !== port || this.connected) return;
        this.lost(port, 'Chrome connection timed out. Reconnecting…');
      }, HANDSHAKE_TIMEOUT);
    } catch (error) {
      this.forgetPort(); this.error = String(error.message ?? error).slice(0, 4000);
      await this.retry();
    }
  }

  lost(port, error) {
    if (this.port !== port) return;
    this.forgetPort(); this.error = error;
    try { port.disconnect(); } catch { /* A failed port may already be closed. */ }
    this.run(() => this.enqueue(async () => {
      // This lifecycle queue fences reconnects behind release of old sessions.
      await this.controller.disconnect();
      await this.retry();
    }));
  }

  async receive(port, message) {
    if (this.port !== port) return;
    if (message?.event === 'connected' && message.protocol_version === 1) {
      await this.enqueue(async () => {
        if (this.port !== port || !this.enabled) return;
        this.connected = true; this.connecting = false; this.error = '';
        this.cancel(this.handshakeTimer); this.handshakeTimer = undefined;
        this.attempt = 0; this.retryAt = 0;
        await this.cancelRetry();
        await this.api.storage.local.set({ connectionRetry: { attempt: 0, at: 0 } });
      });
      return;
    }
    if (typeof message?.id !== 'string' || typeof message?.session_id !== 'string') return;
    await this.ready; await this.lifecycle;
    if (this.port !== port || !this.connected || !this.enabled) return;
    let response;
    try {
      response = { id: message.id, result: await this.controller.call(message.session_id, message.method, message.params ?? {}, message.task_key) };
    } catch (error) { response = { id: message.id, error: String(error.message ?? 'Chrome operation failed.').slice(0, 4000) }; }
    // A result from an old host must never be sent to a newly connected host.
    if (this.port === port && this.connected) {
      try { port.postMessage(response); }
      catch (error) { this.lost(port, String(error.message ?? 'Connection closed.')); }
    }
  }
}

export function installBackground(api) {
  const controller = new ChromeController(api);
  const connection = new NativeConnection(api, controller);
  api.runtime.onMessage.addListener((message, sender, respond) => {
    // Only our own popup may grant/revoke access. No content scripts, external
    // messaging, website origin, tab message or web-accessible pages are exposed.
    if (sender.id !== api.runtime.id || sender.tab || sender.url !== api.runtime.getURL('popup.html')) return false;
    void (async () => {
      await connection.ready;
      if (message.action === 'connect') await connection.connect();
      else if (message.action === 'allowNewTabs' && typeof message.value === 'boolean') {
        await connection.enqueue(async () => {
          controller.allowNewTabs = message.value;
          await api.storage.local.set({ allowNewTabs: message.value });
          if (!message.value) await controller.disconnect();
        });
      } else if (message.action === 'shareCurrent') {
        await connection.enqueue(async () => {
          const [tab] = await api.tabs.query({ active: true, currentWindow: true });
          if (!tab?.id || tab.incognito) throw new Error('Open a normal HTTP(S) page first.');
          await controller.share(tab.id);
        });
      } else if (message.action === 'revoke' && Number.isInteger(message.tab_id)) await connection.enqueue(() => controller.revoke(message.tab_id));
      else if (message.action === 'disconnect') await connection.disconnect();
      else if (message.action !== 'status') throw new Error('Unknown popup action.');
      return { ...connection.status(), allowNewTabs: controller.allowNewTabs, shared: [...controller.shared].map(([tab_id, origin]) => ({ tab_id, origin })) };
    })().then(result => respond({ result }), error => respond({ error: error.message }));
    return true;
  });
  api.tabs.onRemoved.addListener(tabId => { void controller.event(() => controller.removed(tabId)); });
  api.tabs.onUpdated.addListener((tabId, change) => { if (change.groupId !== undefined) void controller.event(() => controller.tabUpdated(tabId)); });
  api.debugger.onDetach.addListener(source => controller.detached(source.tabId));
  api.runtime.onStartup.addListener(() => connection.run(() => connection.resume()));
  api.runtime.onInstalled.addListener(() => connection.run(() => connection.resume()));
  connection.run(() => connection.resume());
  return connection;
}

if (typeof chrome !== 'undefined') installBackground(chrome);
