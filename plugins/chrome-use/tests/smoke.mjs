import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtemp, rm, writeFile, cp, readFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const require = createRequire(path.join(root, 'apps/desktop/package.json'));
const { chromium } = require('playwright-core');
const executablePath = process.env.OPENHARNESS_CHROME_TEST_BINARY;
if (!executablePath) throw new Error('Set OPENHARNESS_CHROME_TEST_BINARY to a test Chromium executable.');
const profile = await mkdtemp(path.join(os.tmpdir(), 'oh-chrome-smoke-'));
let context; let server;
try {
  server = http.createServer((request, response) => {
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    response.end('<!doctype html><title>Chrome background fixture</title><h1>Background test</h1><label>Name <input id="name"></label><button id="go">Submit locally</button><p id="result">Waiting</p><script>go.onclick=()=>{result.textContent="Saved: "+document.querySelector("#name").value}</script>');
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const url = `http://127.0.0.1:${server.address().port}/`;
  const extension = path.join(profile, 'extension');
  await cp(path.join(root, 'plugins/chrome-use/extension'), extension, { recursive: true });
  await writeFile(path.join(extension, 'smoke-background.mjs'), "import { ChromeController } from './controller.mjs'; globalThis.smokeController = new ChromeController(chrome); globalThis.smokeController.allowNewTabs = true;\n");
  const manifest = JSON.parse(await readFile(path.join(extension, 'manifest.json'), 'utf8')); manifest.background.service_worker = 'smoke-background.mjs';
  await writeFile(path.join(extension, 'manifest.json'), JSON.stringify(manifest));
  context = await chromium.launchPersistentContext(profile, { executablePath, headless: true, args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`], timeout: 15000 });
  const worker = context.serviceWorkers()[0] ?? await context.waitForEvent('serviceworker', { timeout: 10000 });
  const page = context.pages()[0] ?? await context.newPage(); await page.goto(`${url}?user=A`);
  const session = '11111111-1111-4111-8111-111111111111';
  const call = (method, args = {}) => worker.evaluate(({ session, method, args }) => globalThis.smokeController.call(session, method, args), { session, method, args });
  const activeBefore = await worker.evaluate(async () => (await chrome.tabs.query({ active: true }))[0].id);
  assert.equal((await call('chrome_tabs')).tabs.length, 0);
  const opened = await call('chrome_open', { url: `${url}?agent=B` }); assert.notEqual(opened.tab_id, activeBefore);
  assert.ok(Number.isInteger(opened.group_id) && opened.group_id >= 0);
  const group = await worker.evaluate(id => chrome.tabGroups.get(id), opened.group_id);
  assert.match(group.title, /Open Harness/);
  assert.equal(await worker.evaluate(async id => (await chrome.tabs.get(id)).groupId, opened.tab_id), opened.group_id);
  const agentPage = context.pages().find(item => item !== page && item.url().includes('agent=B')) ?? await context.waitForEvent('page');
  await agentPage.waitForLoadState('domcontentloaded');
  let snapshot = await call('chrome_snapshot');
  assert.match(snapshot.text, /Background test/);
  const input = snapshot.elements.find(element => element.role === 'input'); assert.ok(input);
  const fill = await call('chrome_action', { revision: snapshot.revision, action: 'fill', ref: input.ref, text: 'MAPLE-4827' });
  assert.equal(fill.dispatched, true); assert.equal(fill.error, undefined); snapshot = fill;
  const button = snapshot.elements.find(element => element.name === 'Submit locally'); assert.ok(button);
  const click = await call('chrome_action', { revision: snapshot.revision, action: 'click', ref: button.ref });
  assert.equal(click.dispatched, true);
  snapshot = await call('chrome_snapshot'); assert.match(snapshot.text, /Saved: MAPLE-4827/);
  const screenshot = await call('chrome_screenshot'); assert.equal(screenshot.image.mimeType, 'image/jpeg'); assert.ok(screenshot.image.data.length > 1000);
  const activeAfter = await worker.evaluate(async () => (await chrome.tabs.query({ active: true }))[0].id);
  assert.equal(activeAfter, activeBefore); assert.equal(await page.locator('#name').inputValue(), '');
  await assert.rejects(call('chrome_snapshot', { tab_id: activeBefore }), /exact tab/);
  const imagePath = path.join(os.tmpdir(), 'openharness-chrome-background-smoke.jpg'); await writeFile(imagePath, Buffer.from(screenshot.image.data, 'base64'));
  const second = await call('chrome_open', { url: `${url}?agent=C` });
  assert.equal(second.group_id, opened.group_id);
  await call('chrome_release', { close_owned: true });
  const otherSession = '22222222-2222-4222-8222-222222222222';
  const other = await worker.evaluate(({ session, url }) => globalThis.smokeController.call(session, 'chrome_open', { url }), { session: otherSession, url: `${url}?task=other` });
  assert.notEqual(other.group_id, opened.group_id);
  await worker.evaluate(session => globalThis.smokeController.call(session, 'chrome_release', { close_owned: true }), otherSession);
  // A user moving their own tab into the task group does not share it or
  // transfer ownership. Closing our tab leaves the user's tab intact.
  await worker.evaluate(({ groupId, tabId }) => chrome.tabs.group({ groupId, tabIds: [tabId] }), { groupId: opened.group_id, tabId: activeBefore });
  assert.equal((await call('chrome_tabs')).tabs.some(tab => tab.tab_id === activeBefore), false);
  await call('chrome_select_tab', { tab_id: opened.tab_id, origin: new URL(url).origin });
  await call('chrome_release', { close_owned: true });
  assert.equal(await worker.evaluate(async id => { try { await chrome.tabs.get(id); return true; } catch { return false; } }, opened.tab_id), false);
  assert.equal(await worker.evaluate(async id => (await chrome.tabs.get(id)).id, activeBefore), activeBefore);
  assert.equal((await worker.evaluate(async () => (await chrome.tabs.query({ active: true }))[0])).id, activeBefore);
  console.log(JSON.stringify({ passed: true, checks: ['unshared tabs private', 'background creation', 'named task group', 'same task reuses group', 'separate tasks have separate groups', 'DOM snapshot', 'fill', 'click', 'result verification', 'background screenshot', 'active user tab unchanged', 'user input untouched', 'cross-tab rejection', 'group membership does not grant access', 'close only owned tab'], screenshot: imagePath }));
} finally {
  try { await context?.close(); }
  finally { if (server) await new Promise(resolve => server.close(resolve)); await rm(profile, { recursive: true, force: true }); }
}
