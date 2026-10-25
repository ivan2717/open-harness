// Real MV3 worker/timer/alarm/storage/popup coverage. Only native messaging is
// simulated so this isolated profile cannot reach a user's registered host.
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile, cp, readFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { EXTENSION_ID } from '../constants.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const require = createRequire(path.join(root, 'apps/desktop/package.json'));
const { chromium } = require('playwright-core');
const executablePath = process.env.OPENHARNESS_CHROME_TEST_BINARY;
if (!executablePath) throw new Error('Set OPENHARNESS_CHROME_TEST_BINARY to a full test Chromium executable.');
const profile = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-background-smoke-'));
async function waitUntil(read, matches) {
  const deadline = Date.now() + 10_000;
  let value;
  do {
    value = await read();
    if (matches(value)) return value;
    await new Promise(resolve => setTimeout(resolve, 50));
  } while (Date.now() < deadline);
  throw new Error(`Condition did not settle: ${JSON.stringify(value)}`);
}
let context;
try {
  const extension = path.join(profile, 'extension');
  await cp(path.join(root, 'plugins/chrome-use/extension'), extension, { recursive: true });
  await writeFile(path.join(extension, 'native-fixture.mjs'), `
const event = () => {
  const listeners = [];
  return { addListener(listener) { listeners.push(listener); }, emit(message) { for (const listener of listeners) listener(message); } };
};
globalThis.nativeFixture = { ports: [] };
// Headless Chrome displays popup.html as a test page rather than a toolbar
// bubble. Normalize only that page's sender to the genuine toolbar shape.
const addPopupListener = chrome.runtime.onMessage.addListener.bind(chrome.runtime.onMessage);
chrome.runtime.onMessage.addListener = listener => addPopupListener((message, sender, respond) => {
  if (sender.id === chrome.runtime.id && sender.url === chrome.runtime.getURL('popup.html')) {
    const { tab, ...popupSender } = sender;
    return listener(message, popupSender, respond);
  }
  return listener(message, sender, respond);
});
chrome.runtime.connectNative = name => {
  if (name !== 'dev.openharness.chrome') throw new Error('Unexpected host');
  const port = {
    onMessage: event(), onDisconnect: event(), messages: [], disconnected: false,
    postMessage(message) { if (this.disconnected) throw new Error('Port closed'); this.messages.push(message); },
    disconnect() { this.disconnected = true; },
    crash() { this.disconnected = true; this.onDisconnect.emit(); },
  };
  globalThis.nativeFixture.ports.push(port);
  queueMicrotask(() => port.onMessage.emit({ event: 'connected', protocol_version: 1 }));
  return port;
};
`);
  // Static imports are supported in extension workers; dynamic import is not.
  // Install the isolated native-port substitute before the complete, unmodified
  // production entry point. Its timers, alarms, storage and popup stay real.
  await writeFile(path.join(extension, 'background-fixture.mjs'), "import './native-fixture.mjs';\nimport './background.mjs';\n");
  const manifest = JSON.parse(await readFile(path.join(extension, 'manifest.json'), 'utf8'));
  manifest.background.service_worker = 'background-fixture.mjs';
  await writeFile(path.join(extension, 'manifest.json'), JSON.stringify(manifest));
  const launch = async () => {
    context = await chromium.launchPersistentContext(profile, {
      executablePath, headless: true,
      args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
      timeout: 15_000,
    });
    const worker = context.serviceWorkers()[0] ?? await context.waitForEvent('serviceworker', { timeout: 10_000 });
    const popup = await context.newPage();
    await popup.goto(`chrome-extension://${EXTENSION_ID}/popup.html`);
    return { worker, popup };
  };
  const status = async (popup, prefix) => {
    try { await popup.waitForFunction(value => document.querySelector('#status')?.textContent.startsWith(value), prefix, { timeout: 10_000 }); }
    catch (error) { throw new Error(`Expected popup status ${prefix}; got ${await popup.locator('#status').textContent()}`, { cause: error }); }
  };
  let { worker, popup } = await launch();
  await status(popup, '已连接');
  assert.equal(await worker.evaluate(() => globalThis.nativeFixture.ports.length), 1);
  // Verify the real browser contract the Node timer mocks could not exercise.
  assert.equal(await worker.evaluate(() => {
    try { ({ timer: clearTimeout }).timer(); return false; }
    catch (error) { return /Illegal invocation/.test(error.message); }
  }), true);
  await worker.evaluate(() => globalThis.nativeFixture.ports[0].crash());
  await waitUntil(() => worker.evaluate(async () => (await chrome.storage.local.get('connectionRetry')).connectionRetry), retry => retry?.attempt > 0);
  assert.ok(await worker.evaluate(async () => !!await chrome.alarms.get('openharness-native-reconnect')));
  await waitUntil(() => worker.evaluate(() => globalThis.nativeFixture.ports.length), count => count === 2);
  await waitUntil(() => worker.evaluate(async () => (await chrome.storage.local.get('connectionRetry')).connectionRetry), retry => retry?.attempt === 0);
  await status(popup, '已连接');
  assert.equal(await worker.evaluate(() => globalThis.nativeFixture.ports.length), 2);
  assert.equal(await worker.evaluate(async () => await chrome.alarms.get('openharness-native-reconnect')), undefined);
  await popup.locator('#disconnect').click(); await status(popup, '已断开');
  assert.equal(await worker.evaluate(async () => (await chrome.storage.local.get('connectionEnabled')).connectionEnabled), false);
  await context.close(); context = undefined;
  ({ worker, popup } = await launch());
  await status(popup, '已断开');
  assert.equal(await worker.evaluate(() => globalThis.nativeFixture.ports.length), 0);
  await popup.locator('#connect').click(); await status(popup, '已连接');
  assert.equal(await worker.evaluate(() => globalThis.nativeFixture.ports.length), 1);
  await context.close(); context = undefined;
  // Model an unpacked upgrade where scripts changed but Chrome still has the
  // old permission set. Initial connection and short retries must remain usable.
  manifest.permissions = manifest.permissions.filter(permission => permission !== 'alarms');
  await writeFile(path.join(extension, 'manifest.json'), JSON.stringify(manifest));
  ({ worker, popup } = await launch());
  await status(popup, '已连接');
  assert.equal(await worker.evaluate(() => chrome.alarms === undefined), true);
  assert.match(await popup.locator('#status').textContent(), /重新加载/);
  await worker.evaluate(() => globalThis.nativeFixture.ports[0].crash());
  await waitUntil(() => worker.evaluate(() => globalThis.nativeFixture.ports.length), count => count === 2);
  await status(popup, '已连接');
  console.log(JSON.stringify({ passed: true, checks: [
    'complete production background initializes in a real MV3 worker',
    'real browser timer receiver regression',
    'unexpected disconnect schedules a real alarm',
    'real timeout reconnects and clears the alarm',
    'popup disconnect persists across browser restart',
    'explicit popup connect resumes transport',
    'old manifest without alarms still initializes and reconnects',
  ] }));
} finally {
  try { await context?.close(); }
  finally { await rm(profile, { recursive: true, force: true }); }
}
