import { validate, allowedUrl } from './schema.mjs';
import { pageOperation } from './page.mjs';
export class ChromeController {
  constructor(api, { id = () => crypto.randomUUID() } = {}) { this.api = api; this.id = id; this.sessions = new Map(); this.owned = new Map(); this.shared = new Map(); this.attached = new Map(); this.contexts = new Map(); this.allowNewTabs = false; this.queue = Promise.resolve(); this.generation = 0; }
  call(sessionId, method, args = {}) {
    const generation = this.generation;
    const operation = this.queue.then(() => { if (generation !== this.generation) throw new Error('Chrome connection was cancelled.'); return this.perform(sessionId, method, args); });
    this.queue = operation.catch(() => {}); return operation;
  }
  async perform(sessionId, method, args) {
    if (!/^[\da-f-]{36}$/.test(sessionId)) throw new Error('Invalid browser session.');
    if (method === 'disconnect') { await this.releaseSession(sessionId); return { released: true }; }
    validate(method, args);
    if (method === 'chrome_status') return { connected: true, allow_new_tabs: this.allowNewTabs, shared_tabs: this.shared.size, background_tabs: true };
    if (method === 'chrome_tabs') {
      const tabs = [];
      for (const tab of await this.api.tabs.query({})) {
        const owned = this.owned.get(tab.id);
        const sharedOrigin = this.shared.get(tab.id);
        if ((!owned || owned.sessionId !== sessionId) && !sharedOrigin) continue;
        let origin; try { origin = allowedUrl(tab.url).origin; } catch { continue; }
        // Do not leak titles or URLs after the user navigates outside a grant.
        if (owned?.sessionId === sessionId && origin !== owned.origin) {
          tabs.push({ tab_id: tab.id, origin, owned: true, reauthorization_required: true }); continue;
        }
        if (origin !== (owned?.sessionId === sessionId ? owned.origin : sharedOrigin)) continue;
        tabs.push({ tab_id: tab.id, origin, url: tab.url, title: String(tab.title ?? '').slice(0, 500), owned: owned?.sessionId === sessionId, busy: this.attached.has(tab.id) && this.attached.get(tab.id) !== sessionId });
      }
      return { tabs };
    }
    if (method === 'chrome_open') {
      if (!this.allowNewTabs) throw new Error('Enable background tabs in the Open Harness Chrome extension popup first.');
      const url = allowedUrl(args.url);
      const tab = await this.api.tabs.create({ url: url.href, active: false });
      this.owned.set(tab.id, { sessionId, origin: url.origin });
      await this.releaseSession(sessionId);
      await this.api.debugger.attach({ tabId: tab.id }, '1.3');
      this.attached.set(tab.id, sessionId); this.sessions.set(sessionId, { tabId: tab.id, origin: url.origin, revision: null });
      return { tab_id: tab.id, origin: url.origin, url: url.href, created: true, selected: true };
    }
    if (method === 'chrome_select_tab') {
      const tab = await this.api.tabs.get(args.tab_id); const origin = allowedUrl(tab.url).origin;
      const owned = this.owned.get(tab.id);
      if (origin !== args.origin) throw new Error('origin_changed: use the actual current origin and request a new grant.');
      if (owned?.sessionId !== sessionId && this.shared.get(tab.id) !== origin) throw new Error('Share this exact tab and origin from the Open Harness Chrome extension popup first.');
      const owner = this.attached.get(tab.id);
      if (owner && owner !== sessionId) throw new Error('Tab is already controlled by another session.');
      await this.releaseSession(sessionId);
      try { await this.api.debugger.attach({ tabId: tab.id }, '1.3'); } catch { throw new Error('Chrome could not attach to this tab. Close its DevTools or reconnect after browser policy checks.'); }
      if (owned?.sessionId === sessionId) owned.origin = origin;
      this.attached.set(tab.id, sessionId); this.sessions.set(sessionId, { tabId: tab.id, origin, revision: null });
      return this.snapshot(sessionId);
    }
    if (method === 'chrome_release') {
      const session = this.sessions.get(sessionId);
      if (!session || (args.tab_id !== undefined && args.tab_id !== session.tabId)) throw new Error('Select and authorize this exact tab first.');
      const owned = this.owned.get(session.tabId);
      await this.releaseSession(sessionId);
      if (args.close_owned && owned?.sessionId === sessionId) { await this.api.tabs.remove(session.tabId); this.owned.delete(session.tabId); }
      return { released: true, closed: !!args.close_owned && owned?.sessionId === sessionId };
    }
    const session = await this.selected(sessionId, args.tab_id);
    if (method === 'chrome_snapshot') return this.snapshot(sessionId, args.text_offset);
    if (method === 'chrome_screenshot') {
      const snapshot = await this.snapshot(sessionId);
      const result = await this.command(session.tabId, 'Page.captureScreenshot', { format: 'jpeg', quality: 70, fromSurface: true, captureBeyondViewport: false });
      await this.selected(sessionId);
      if (typeof result.data !== 'string' || result.data.length > 6 * 1024 * 1024) throw new Error('Screenshot is too large.');
      return { ...snapshot, image: { mimeType: 'image/jpeg', data: result.data } };
    }
    if (args.revision !== session.revision || !session.revision) throw new Error('stale_snapshot: take a fresh snapshot.');
    session.revision = null; // Consume before dispatch; failed effects must not be retried blindly.
    const operationArgs = { ...args, version: session.version };
    if (args.action === 'click') {
      const point = await this.runPage(session, 'click', operationArgs);
      await this.selected(sessionId);
      await this.command(session.tabId, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: point.x, y: point.y, button: 'left', clickCount: 1 });
      await this.command(session.tabId, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x: point.x, y: point.y, button: 'left', clickCount: 1 });
    } else if (args.action === 'press') {
      await this.runPage(session, 'focus', operationArgs); await this.selected(sessionId);
      const codes = { Enter: 13, Tab: 9, Escape: 27, ArrowDown: 40, ArrowUp: 38, ArrowLeft: 37, ArrowRight: 39, Backspace: 8 };
      await this.command(session.tabId, 'Input.dispatchKeyEvent', { type: 'keyDown', key: args.key, windowsVirtualKeyCode: codes[args.key], ...(args.key === 'Enter' ? { text: '\r' } : {}) });
      await this.command(session.tabId, 'Input.dispatchKeyEvent', { type: 'keyUp', key: args.key, windowsVirtualKeyCode: codes[args.key] });
    } else await this.runPage(session, args.action, operationArgs);
    try { return { dispatched: true, ...await this.snapshot(sessionId) }; }
    catch (error) { return { dispatched: true, verified: false, error: error.message, tab_id: session.tabId }; }
  }
  async selected(sessionId, requestedTab) {
    const session = this.sessions.get(sessionId);
    if (!session || (requestedTab !== undefined && requestedTab !== session.tabId)) throw new Error('Select and authorize this exact tab first.');
    const tab = await this.api.tabs.get(session.tabId);
    const origin = allowedUrl(tab.url).origin;
    if (origin !== session.origin) throw new Error(`origin_changed: current origin is ${origin}; request authorization and select again. Existing shared tabs also require a new popup share.`);
    const owned = this.owned.get(session.tabId);
    if (!(owned?.sessionId === sessionId && owned.origin === session.origin) && this.shared.get(session.tabId) !== session.origin) throw new Error('Tab sharing was revoked.');
    return session;
  }
  command(tabId, method, params = {}) { return this.api.debugger.sendCommand({ tabId }, method, params); }
  async runPage(session, operation, args) {
    const tree = await this.command(session.tabId, 'Page.getFrameTree');
    const frameId = tree.frameTree.frame.id;
    const world = await this.command(session.tabId, 'Page.createIsolatedWorld', { frameId, worldName: 'open-harness-chrome', grantUniveralAccess: false });
    if (operation !== 'snapshot' && session.contextId !== world.executionContextId) throw new Error('stale_snapshot: navigation changed the document.');
    const result = await this.command(session.tabId, 'Runtime.callFunctionOn', { executionContextId: world.executionContextId, functionDeclaration: pageOperation.toString(), arguments: [{ value: operation }, { value: args }], returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description?.slice(0, 500) ?? 'Page operation failed.');
    session.contextId = world.executionContextId;
    return result.result.value;
  }
  async snapshot(sessionId, textOffset = 0) {
    const session = await this.selected(sessionId);
    const snapshot = await this.runPage(session, 'snapshot', { text_offset: textOffset });
    await this.selected(sessionId);
    if (allowedUrl(snapshot.url).origin !== session.origin) throw new Error('origin_changed: snapshot discarded.');
    session.version = snapshot.version; session.revision = this.id();
    const { version, ...content } = snapshot;
    return { tab_id: session.tabId, origin: session.origin, revision: session.revision, ...content };
  }
  async releaseSession(sessionId) {
    const session = this.sessions.get(sessionId); if (!session) return;
    this.sessions.delete(sessionId); this.attached.delete(session.tabId);
    await this.api.debugger.detach({ tabId: session.tabId }).catch(() => {});
  }
  async share(tabId) { const tab = await this.api.tabs.get(tabId); const origin = allowedUrl(tab.url).origin; this.shared.set(tabId, origin); return { tab_id: tabId, origin }; }
  async revoke(tabId) { this.shared.delete(tabId); const owner = this.attached.get(tabId); if (owner) await this.releaseSession(owner); }
  async disconnect() { this.generation++; for (const session of [...this.sessions.keys()]) await this.releaseSession(session); this.owned.clear(); this.shared.clear(); }
  detached(tabId) { const owner = this.attached.get(tabId); if (owner) this.sessions.delete(owner); this.attached.delete(tabId); }
}
