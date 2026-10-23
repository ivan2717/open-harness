import test from 'node:test';
import assert from 'node:assert/strict';
import { ChromeController } from '../extension/controller.mjs';
const A = '11111111-1111-4111-8111-111111111111';
const B = '22222222-2222-4222-8222-222222222222';
function fixture() {
  const pages = new Map([[1, { id: 1, url: 'https://private.example/inbox', title: 'Private inbox', active: true }], [2, { id: 2, url: 'https://shared.example/start', title: 'Shared', active: false }]]);
  const commands = []; const created = []; const detached = []; const removed = []; let nextId = 3; let revision = 0;
  const api = {
    tabs: {
      async query() { return [...pages.values()]; },
      async get(id) { if (!pages.has(id)) throw new Error('Missing tab'); return pages.get(id); },
      async create(args) { created.push(args); const page = { id: nextId++, ...args, title: 'Agent page' }; pages.set(page.id, page); return page; },
      async remove(id) { removed.push(id); pages.delete(id); },
    },
    debugger: {
      async attach(target) { commands.push({ target, method: 'attach' }); },
      async detach(target) { detached.push(target.tabId); },
      async sendCommand(target, method, args) {
        commands.push({ target, method, args });
        if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: `frame-${target.tabId}` } } };
        if (method === 'Page.createIsolatedWorld') return { executionContextId: target.tabId * 100 };
        if (method === 'Page.captureScreenshot') return { data: 'aW1hZ2U=' };
        if (method === 'Runtime.callFunctionOn') {
          const operation = args.arguments[0].value;
          if (operation === 'snapshot') return { result: { value: { version: 0, url: pages.get(target.tabId).url, title: 'Observed', text: 'hello', elements: [{ ref: 'e1', role: 'button', name: 'Next' }] } } };
          return { result: { value: { x: 20, y: 30, dispatched: true } } };
        }
        return {};
      },
    },
  };
  const controller = new ChromeController(api, { id: () => `revision-${++revision}` });
  return { controller, pages, commands, created, detached, removed };
}
test('private tabs are never enumerated; sharing exposes only the current exact origin', async () => {
  const { controller, pages } = fixture();
  assert.deepEqual(await controller.call(A, 'chrome_tabs'), { tabs: [] });
  await controller.share(2);
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs.map(tab => tab.tab_id), [2]);
  pages.get(2).url = 'https://other.example/secret';
  assert.deepEqual(await controller.call(A, 'chrome_tabs'), { tabs: [] });
});
test('new tabs require explicit extension opt-in and are created in background without changing user tab', async () => {
  const { controller, pages, created, commands } = fixture();
  await assert.rejects(controller.call(A, 'chrome_open', { url: 'https://example.com' }), /Enable background/);
  controller.allowNewTabs = true;
  const result = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  assert.equal(result.selected, true); assert.equal(result.origin, 'https://example.com');
  assert.deepEqual(created, [{ url: 'https://example.com/', active: false }]);
  assert.equal(pages.get(1).active, true);
  assert.deepEqual(commands.map(item => item.method), ['attach']); // Opening does not read page contents.
  const snapshot = await controller.call(A, 'chrome_snapshot'); assert.equal(snapshot.tab_id, result.tab_id);
});
test('session ownership and explicit tab targets prevent cross-session or cross-tab access', async () => {
  const { controller } = fixture(); controller.allowNewTabs = true;
  const result = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: result.tab_id, origin: result.origin }), /Share this exact tab/);
  await assert.rejects(controller.call(A, 'chrome_snapshot', { tab_id: 1 }), /exact tab/);
  assert.deepEqual(await controller.call(B, 'chrome_tabs'), { tabs: [] });
});
test('shared tab has one owner, revocation immediately detaches and rejects further reads', async () => {
  const { controller, detached } = fixture(); await controller.share(2);
  await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' }), /another session/);
  await controller.revoke(2); assert.deepEqual(detached, [2]);
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /Select and authorize/);
});
test('cross-origin navigation fails before any page observation or input', async () => {
  const { controller, pages, commands } = fixture(); await controller.share(2);
  const snapshot = await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  const count = commands.length; pages.get(2).url = 'https://private.example/inbox';
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /origin_changed/);
  await assert.rejects(controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: snapshot.revision }), /origin_changed/);
  assert.equal(commands.length, count);
});
test('actions consume revisions and dispatch to exact background tab; screenshot stays tab-scoped', async () => {
  const { controller, commands } = fixture(); await controller.share(2);
  const before = await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  const after = await controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: before.revision });
  assert.equal(after.dispatched, true); assert.notEqual(after.revision, before.revision);
  assert.deepEqual(commands.filter(call => call.method === 'Input.dispatchMouseEvent').map(call => call.target), [{ tabId: 2 }, { tabId: 2 }]);
  await assert.rejects(controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: before.revision }), /stale_snapshot/);
  const image = await controller.call(A, 'chrome_screenshot'); assert.equal(image.image.mimeType, 'image/jpeg');
  assert.equal(commands.find(call => call.method === 'Page.captureScreenshot').target.tabId, 2);
});
test('release never closes user tabs and disconnect invalidates all grants', async () => {
  const { controller, removed } = fixture(); await controller.share(2);
  await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  await controller.call(A, 'chrome_release', { close_owned: true }); assert.deepEqual(removed, []);
  controller.allowNewTabs = true;
  const tab = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  await controller.call(A, 'chrome_release', { close_owned: true }); assert.deepEqual(removed, [tab.tab_id]);
  await controller.disconnect(); assert.equal(controller.shared.size, 0); assert.equal(controller.owned.size, 0);
});
test('arbitrary JavaScript, browser URLs and unknown methods are rejected', async () => {
  const { controller } = fixture(); controller.allowNewTabs = true;
  for (const url of ['javascript:alert(1)', 'file:///etc/passwd', 'chrome://settings', 'https://user:pass@example.com']) await assert.rejects(controller.call(A, 'chrome_open', { url }));
  await assert.rejects(controller.call(A, 'chrome_evaluate', { code: '1+1' }));
  await assert.rejects(controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: 'old', expression: '1+1' }));
});
test('owned cross-origin tabs expose only the new origin until separately selected, without a popup deadlock', async () => {
  const { controller, pages, detached } = fixture(); controller.allowNewTabs = true;
  const opened = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  pages.get(opened.tab_id).url = 'https://other.example/secret-path'; pages.get(opened.tab_id).title = 'Private new title';
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /current origin is https:\/\/other.example/);
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs, [{ tab_id: opened.tab_id, origin: 'https://other.example', owned: true, reauthorization_required: true }]);
  const rebound = await controller.call(A, 'chrome_select_tab', { tab_id: opened.tab_id, origin: 'https://other.example' });
  assert.equal(rebound.origin, 'https://other.example'); assert.ok(detached.includes(opened.tab_id));
  pages.get(opened.tab_id).url = 'https://third.example/';
  assert.equal((await controller.call(A, 'chrome_release')).released, true); // Detaching cannot be blocked by navigation.
});
