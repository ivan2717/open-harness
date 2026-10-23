import { ChromeController } from './controller.mjs';
const controller = new ChromeController(chrome);
let port; let connected = false; let connectionError = ''; let connecting = false;
const ready = chrome.storage.local.get('allowNewTabs').then(settings => { controller.allowNewTabs = settings.allowNewTabs === true; });
function connect() {
  if (port || connecting) return;
  connecting = true; connectionError = '';
  try {
    port = chrome.runtime.connectNative('dev.openharness.chrome');
    port.onMessage.addListener(async message => {
      if (message?.event === 'connected' && message.protocol_version === 1) { connected = true; connecting = false; return; }
      if (typeof message?.id !== 'string' || typeof message?.session_id !== 'string') return;
      const current = port;
      try {
        await ready;
        const result = await controller.call(message.session_id, message.method, message.params ?? {});
        current?.postMessage({ id: message.id, result });
      } catch (error) { current?.postMessage({ id: message.id, error: String(error.message ?? 'Chrome operation failed.').slice(0, 4000) }); }
    });
    port.onDisconnect.addListener(() => {
      connectionError = chrome.runtime.lastError?.message ?? 'Connection closed.';
      port = undefined; connected = false; connecting = false;
      void controller.disconnect();
    });
  } catch (error) { port = undefined; connecting = false; connectionError = error.message; }
}
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  // Only our own popup may grant/revoke access. No content scripts, external
  // messaging, website origin, tab message or web-accessible pages are exposed.
  if (sender.id !== chrome.runtime.id || sender.tab || sender.url !== chrome.runtime.getURL('popup.html')) return false;
  void (async () => {
    await ready;
    if (message.action === 'connect') connect();
    else if (message.action === 'allowNewTabs' && typeof message.value === 'boolean') {
      controller.allowNewTabs = message.value;
      await chrome.storage.local.set({ allowNewTabs: message.value });
      if (!message.value) await controller.disconnect();
    } else if (message.action === 'shareCurrent') {
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      if (!tab?.id || tab.incognito) throw new Error('Open a normal HTTP(S) page first.');
      await controller.share(tab.id);
    } else if (message.action === 'revoke' && Number.isInteger(message.tab_id)) await controller.revoke(message.tab_id);
    else if (message.action === 'disconnect') { port?.disconnect(); await controller.disconnect(); }
    else if (!['status', 'connect'].includes(message.action)) throw new Error('Unknown popup action.');
    return { connected, connecting, error: connectionError, allowNewTabs: controller.allowNewTabs, shared: [...controller.shared].map(([tab_id, origin]) => ({ tab_id, origin })) };
  })().then(result => respond({ result }), error => respond({ error: error.message }));
  return true;
});
chrome.tabs.onRemoved.addListener(tabId => { controller.shared.delete(tabId); controller.owned.delete(tabId); controller.detached(tabId); });
chrome.debugger.onDetach.addListener(source => controller.detached(source.tabId));
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
