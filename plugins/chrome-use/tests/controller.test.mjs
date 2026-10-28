import test from 'node:test';
import assert from 'node:assert/strict';
import { ChromeController } from '../extension/controller.mjs';
import { pageOperation } from '../extension/page.mjs';
import { runInNewContext } from 'node:vm';
const A = '11111111-1111-4111-8111-111111111111';
const B = '22222222-2222-4222-8222-222222222222';
function fixture() {
  const pages = new Map([[1, { id: 1, windowId: 1, groupId: -1, url: 'https://private.example/inbox', title: 'Private inbox', active: true }], [2, { id: 2, windowId: 1, groupId: -1, url: 'https://shared.example/start', title: 'Shared', active: false }]]);
  const groups = new Map(); const stored = {};
  const windows = new Map([[1, { id: 1, type: 'normal', focused: true, incognito: false }]]); const createdWindows = [];
  const commands = []; const created = []; const detached = []; const removed = []; let nextId = 3; let nextGroupId = 20; let revision = 0;
  const pruneGroups = () => { for (const id of groups.keys()) if (![...pages.values()].some(tab => tab.groupId === id)) groups.delete(id); };
  const api = {
    windows: {
      async getAll({ windowTypes }) { return [...windows.values()].filter(window => windowTypes.includes(window.type)); },
      async create(args) {
        createdWindows.push(args);
        const window = { id: 100 + createdWindows.length, ...args };
        const tab = { id: nextId++, windowId: window.id, groupId: -1, url: args.url, active: true, incognito: args.incognito, title: 'Agent page' };
        pages.set(tab.id, tab); windows.set(window.id, window);
        return { ...window, tabs: [tab] };
      },
    },
    tabs: {
      async query(filter = {}) { return [...pages.values()].filter(tab => Object.entries(filter).every(([key, value]) => key === 'currentWindow' || tab[key] === value)); },
      async get(id) { if (!pages.has(id)) throw new Error('Missing tab'); return pages.get(id); },
      async create(args) { created.push(args); const page = { id: nextId++, windowId: 1, groupId: -1, ...args, title: 'Agent page' }; pages.set(page.id, page); return page; },
      async remove(id) { removed.push(id); pages.delete(id); pruneGroups(); },
      async group({ groupId, tabIds, createProperties }) {
        const ids = Array.isArray(tabIds) ? tabIds : [tabIds];
        if (groupId === undefined) {
          groupId = nextGroupId++;
          groups.set(groupId, { id: groupId, windowId: createProperties?.windowId ?? pages.get(ids[0]).windowId, title: '', color: 'grey', collapsed: false });
        }
        const group = groups.get(groupId); if (!group) throw new Error('Missing group');
        for (const id of ids) { const tab = pages.get(id); if (!tab) throw new Error('Missing tab'); tab.groupId = groupId; tab.windowId = group.windowId; }
        pruneGroups(); return groupId;
      },
    },
    tabGroups: {
      async get(id) { if (!groups.has(id)) throw new Error('Missing group'); return { ...groups.get(id) }; },
      async update(id, change) { if (!groups.has(id)) throw new Error('Missing group'); Object.assign(groups.get(id), change); return { ...groups.get(id) }; },
    },
    storage: {
      session: {
        async get(key) { return structuredClone(key === null ? stored : { [key]: stored[key] }); },
        async set(value) { Object.assign(stored, structuredClone(value)); },
      },
    },
    debugger: {
      async attach(target) { commands.push({ target, method: 'attach' }); },
      async detach(target) { detached.push(target.tabId); },
      async sendCommand(target, method, args) {
        commands.push({ target, method, args });
        if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: `frame-${target.tabId}`, url: pages.get(target.tabId).url } } };
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
  return { controller, api, pages, groups, stored, commands, created, detached, removed, windows, createdWindows };
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
  const { controller, pages, created, commands, createdWindows } = fixture();
  await assert.rejects(controller.call(A, 'chrome_open', { url: 'https://example.com' }), /Enable background/);
  controller.allowNewTabs = true;
  const result = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  assert.equal(result.selected, true); assert.equal(result.origin, 'https://example.com');
  assert.equal(created[0].url, 'https://example.com/'); assert.equal(created[0].active, false);
  assert.equal(pages.get(1).active, true);
  assert.deepEqual(createdWindows, []);
  assert.deepEqual(commands.map(item => item.method), ['attach']); // Opening does not read page contents.
  const snapshot = await controller.call(A, 'chrome_snapshot'); assert.equal(snapshot.tab_id, result.tab_id);
});
test('a browser with no normal window creates one unfocused task window and protects its active tab', async () => {
  const { controller, windows, pages, created, createdWindows, groups, removed } = fixture();
  windows.clear(); pages.clear(); controller.allowNewTabs = true;
  const result = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  assert.deepEqual(createdWindows, [{ url: 'https://example.com/', type: 'normal', focused: false, incognito: false }]);
  assert.deepEqual(created, []); // Never create the requested page twice.
  assert.equal(pages.get(result.tab_id).windowId, 101);
  assert.equal(groups.get(result.group_id).windowId, 101);
  assert.equal(windows.get(101).focused, false);
  assert.equal((await controller.call(A, 'chrome_snapshot')).tab_id, result.tab_id);
  const released = await controller.call(A, 'chrome_release', { close_owned: true });
  assert.equal(released.closed, false); assert.deepEqual(removed, []);
  const next = await controller.call(A, 'chrome_open', { url: 'https://example.com/next' });
  assert.equal(next.group_id, result.group_id); assert.equal(createdWindows.length, 1);
  assert.equal(created[0].active, false); assert.equal(created[0].windowId, 101);
});
test('only private or popup windows cause a separate normal unfocused window', async () => {
  const { controller, windows, createdWindows, pages } = fixture(); controller.allowNewTabs = true;
  windows.clear(); windows.set(1, { id: 1, type: 'normal', incognito: true, focused: true }); windows.set(2, { id: 2, type: 'popup', incognito: false });
  const result = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  assert.equal(createdWindows.length, 1); assert.equal(pages.get(result.tab_id).windowId, 101);
  assert.equal(pages.get(1).windowId, 1); assert.equal(pages.get(1).active, true);
});
test('an unidentified newly created window fails without opening another page or attaching', async () => {
  const { controller, api, windows, commands, created } = fixture(); controller.allowNewTabs = true; windows.clear();
  let creations = 0; api.windows.create = async () => { creations++; return { id: 101 }; };
  await assert.rejects(controller.call(A, 'chrome_open', { url: 'https://example.com' }), /Created Chrome window 101.*do not repeat chrome_open/);
  assert.equal(creations, 1); assert.deepEqual(created, []); assert.deepEqual(commands, []);
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
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs, [{ tab_id: opened.tab_id, origin: 'https://other.example', owned: true, selection_required: true }]);
  const rebound = await controller.call(A, 'chrome_select_tab', { tab_id: opened.tab_id, origin: 'https://other.example' });
  assert.equal(rebound.origin, 'https://other.example'); assert.ok(detached.includes(opened.tab_id));
  pages.get(opened.tab_id).url = 'https://third.example/';
  assert.equal((await controller.call(A, 'chrome_release')).released, true); // Detaching cannot be blocked by navigation.
});

test('one task reuses its named background group while different tasks stay separate', async () => {
  const { controller, pages, groups, created } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/one' });
  const second = await controller.call(A, 'chrome_open', { url: 'https://example.com/two' });
  const other = await controller.call(B, 'chrome_open', { url: 'https://example.com/other' });
  assert.ok(Number.isInteger(first.group_id)); assert.equal(first.group_id, second.group_id);
  assert.notEqual(first.group_id, other.group_id);
  assert.match(groups.get(first.group_id).title, /Open Harness/);
  assert.equal(pages.get(first.tab_id).groupId, first.group_id);
  assert.equal(pages.get(second.tab_id).groupId, first.group_id);
  assert.equal(created.every(tab => tab.active === false), true);
  assert.equal(pages.get(1).active, true); assert.equal(pages.get(1).groupId, -1);
});

test('delegated sessions may use released family tabs without stealing or closing a teammate tab', async () => {
  const { controller, removed } = fixture(); controller.allowNewTabs = true;
  const root = 'c'.repeat(64);
  const opened = await controller.call(A, 'chrome_open', { url: 'https://example.com/work' }, root);
  assert.deepEqual((await controller.call(B, 'chrome_tabs', {}, 'd'.repeat(64))).tabs, []);
  const siblingTabs = (await controller.call(B, 'chrome_tabs', {}, root)).tabs;
  assert.equal(siblingTabs.length, 1); assert.equal(siblingTabs[0].busy, true);
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: opened.tab_id, origin: opened.origin }, root), /already controlled/);
  await controller.call(A, 'chrome_release', {}, root);
  const selected = await controller.call(B, 'chrome_select_tab', { tab_id: opened.tab_id, origin: opened.origin }, root);
  assert.equal(selected.tab_id, opened.tab_id);
  assert.equal((await controller.call(B, 'chrome_snapshot', {}, root)).tab_id, opened.tab_id);
  const released = await controller.call(B, 'chrome_release', { close_owned: true }, root);
  assert.equal(released.closed, false); assert.deepEqual(removed, []);
});

test('family tasks recover a navigated owned tab without sharing or opening another page', async () => {
  const { controller, pages, created, commands, removed } = fixture(); controller.allowNewTabs = true;
  const task = 'c'.repeat(64);
  const opened = await controller.call(A, 'chrome_open', { url: 'https://example.com/start' }, task);
  const old = await controller.call(A, 'chrome_snapshot', {}, task);
  pages.get(opened.tab_id).url = 'https://other.example/result';
  const changed = (await controller.call(B, 'chrome_tabs', {}, task)).tabs;
  assert.deepEqual(changed, [{ tab_id: opened.tab_id, origin: 'https://other.example', owned: false, selection_required: true }]);
  assert.deepEqual((await controller.call(B, 'chrome_tabs', {}, 'd'.repeat(64))).tabs, []);
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: opened.tab_id, origin: changed[0].origin }, task), /already controlled/);
  await controller.call(A, 'chrome_release', {}, task);
  const fresh = await controller.call(B, 'chrome_select_tab', { tab_id: opened.tab_id, origin: changed[0].origin }, task);
  assert.equal(fresh.origin, 'https://other.example'); assert.equal(fresh.tab_id, opened.tab_id);
  assert.equal(controller.shared.size, 0); assert.equal(created.length, 1);
  await assert.rejects(controller.call(B, 'chrome_action', { action: 'click', ref: 'e1', revision: old.revision }, task), /stale_snapshot/);
  assert.equal(commands.some(command => command.method === 'Input.dispatchMouseEvent'), false);
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: 1, origin: 'https://private.example' }, task), /Share this exact tab/);
  assert.equal((await controller.call(B, 'chrome_release', { close_owned: true }, task)).closed, false);
  assert.deepEqual(removed, []);
});

test('task layout survives native reconnect without restoring page grants or ownership', async () => {
  const { controller, pages, removed } = fixture(); controller.allowNewTabs = true;
  const task = 'a'.repeat(64);
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/old' }, task);
  await controller.disconnect();
  assert.deepEqual(await controller.call(B, 'chrome_tabs', {}, task), { tabs: [] });
  await assert.rejects(controller.call(B, 'chrome_select_tab', { tab_id: first.tab_id, origin: 'https://example.com' }, task), /Share this exact tab/);
  const second = await controller.call(B, 'chrome_open', { url: 'https://example.com/new' }, task);
  assert.equal(second.group_id, first.group_id);
  await controller.call(B, 'chrome_release', { close_owned: true }, task);
  assert.deepEqual(removed, [second.tab_id]); assert.equal(pages.has(first.tab_id), true);
});

test('worker recreation restores task group layout from session storage only', async () => {
  const { controller, api, pages } = fixture(); controller.allowNewTabs = true;
  const task = 'b'.repeat(64);
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/old' }, task);
  await controller.disconnect();
  const restarted = new ChromeController(api); restarted.allowNewTabs = true;
  const second = await restarted.call(B, 'chrome_open', { url: 'https://example.com/new' }, task);
  assert.equal(second.group_id, first.group_id);
  assert.deepEqual((await restarted.call(B, 'chrome_tabs', {}, task)).tabs.map(tab => tab.tab_id), [second.tab_id]);
  assert.equal(pages.has(first.tab_id), true);
});

test('selecting a shared user tab leaves its position and group unchanged and cannot close it', async () => {
  const { controller, api, pages, removed } = fixture(); controller.allowNewTabs = true;
  const userGroup = await api.tabs.group({ tabIds: [1, 2] });
  await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  await controller.share(2);
  await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  assert.equal(pages.get(2).groupId, userGroup);
  const result = await controller.call(A, 'chrome_release', { close_owned: true });
  assert.equal(result.closed, false); assert.deepEqual(removed, []);
  assert.equal(pages.get(2).groupId, userGroup);
});

test('user moving an unrelated tab into the agent group grants no read or close access', async () => {
  const { controller, api, pages, removed } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  await api.tabs.group({ groupId: first.group_id, tabIds: [1] });
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs.map(tab => tab.tab_id), [first.tab_id]);
  await assert.rejects(controller.call(A, 'chrome_select_tab', { tab_id: 1, origin: 'https://private.example' }), /Share this exact tab/);
  await controller.call(A, 'chrome_release', { close_owned: true });
  assert.deepEqual(removed, [first.tab_id]); assert.equal(pages.has(1), true);
});

test('removed groups are recreated without changing unrelated tabs', async () => {
  const { controller, api, groups, pages } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/one' });
  await api.tabs.remove(first.tab_id); await controller.removed(first.tab_id);
  assert.equal(groups.has(first.group_id), false);
  const second = await controller.call(A, 'chrome_open', { url: 'https://example.com/two' });
  assert.notEqual(second.group_id, first.group_id); assert.equal(pages.get(1).groupId, -1);
});

test('a stale saved group with no known anchor is not reused for newly opened tabs', async () => {
  const { controller, pages, groups } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/one' });
  // Simulate the user taking every known agent tab out and putting their own tab
  // in the group before the queued tab event is handled.
  pages.get(first.tab_id).groupId = -1; pages.get(1).groupId = first.group_id;
  groups.get(first.group_id).title = 'My tabs';
  const second = await controller.call(A, 'chrome_open', { url: 'https://example.com/two' });
  assert.notEqual(second.group_id, first.group_id);
  assert.equal(groups.get(first.group_id).title, 'My tabs'); assert.equal(pages.get(1).groupId, first.group_id);
});

test('moving a controlled agent tab out of its group revokes control and close ownership', async () => {
  const { controller, pages, removed, detached } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  pages.get(first.tab_id).groupId = -1;
  await controller.tabUpdated(first.tab_id, { groupId: -1 });
  assert.ok(detached.includes(first.tab_id)); assert.equal(controller.owned.has(first.tab_id), false);
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /Select and authorize|sharing|group/i);
  await assert.rejects(controller.call(A, 'chrome_release', { close_owned: true }), /Select and authorize|sharing|group/i);
  assert.deepEqual(removed, []); assert.equal(pages.has(first.tab_id), true);
});

test('close protection checks current group even before the move event is processed', async () => {
  const { controller, pages, removed } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  pages.get(first.tab_id).groupId = -1;
  const result = await controller.call(A, 'chrome_release', { close_owned: true });
  assert.equal(result.closed, false); assert.deepEqual(removed, []); assert.equal(pages.has(first.tab_id), true);
});

test('an agent tab the user is currently viewing is released without being closed', async () => {
  const { controller, pages, removed } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  pages.get(1).active = false; pages.get(first.tab_id).active = true;
  const result = await controller.call(A, 'chrome_release', { close_owned: true });
  assert.equal(result.closed, false); assert.deepEqual(removed, []); assert.equal(pages.has(first.tab_id), true);
});

test('future task tabs follow their group across windows without activating it', async () => {
  const { controller, pages, groups, created } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com/one' });
  groups.get(first.group_id).windowId = 5; pages.get(first.tab_id).windowId = 5;
  const second = await controller.call(A, 'chrome_open', { url: 'https://example.com/two' });
  assert.equal(second.group_id, first.group_id); assert.equal(created[1].windowId, 5);
  assert.equal(created[1].active, false); assert.equal(pages.get(1).windowId, 1); assert.equal(pages.get(1).active, true);
});

test('navigating an agent tab to an unapproved site protects it from closing', async () => {
  const { controller, pages, removed } = fixture(); controller.allowNewTabs = true;
  const first = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  pages.get(first.tab_id).url = 'https://private.example/inbox';
  const result = await controller.call(A, 'chrome_release', { close_owned: true });
  assert.equal(result.closed, false); assert.deepEqual(removed, []); assert.equal(pages.has(first.tab_id), true);
});

test('grouping failure rolls back only the new tab and never attaches a debugger', async () => {
  const { controller, api, pages, removed, commands } = fixture(); controller.allowNewTabs = true;
  api.tabs.group = async () => { throw new Error('Grouping unavailable'); };
  await assert.rejects(controller.call(A, 'chrome_open', { url: 'https://example.com' }), /Grouping|group/i);
  assert.deepEqual(removed, [3]); assert.deepEqual([...pages.keys()], [1, 2]);
  assert.equal(controller.owned.size, 0); assert.equal(controller.sessions.size, 0);
  assert.equal(commands.some(command => command.method === 'attach'), false);
});

test('attachment failure identifies the existing grouped tab so it need not be created twice', async () => {
  const { controller, api, pages } = fixture(); controller.allowNewTabs = true;
  api.debugger.attach = async () => { throw new Error('DevTools busy'); };
  await assert.rejects(controller.call(A, 'chrome_open', { url: 'https://example.com' }), /Created tab 3 in group .*do not repeat chrome_open/);
  assert.equal(controller.sessions.size, 0);
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs.map(tab => tab.tab_id), [3]);
  assert.notEqual(pages.get(3).groupId, -1);
});

test('a new uncommitted tab reports loading and retries only reads without creating another tab', async () => {
  const { controller, api, pages, commands, created } = fixture(); controller.allowNewTabs = true;
  const create = api.tabs.create;
  api.tabs.create = async args => {
    const tab = await create(args);
    Object.assign(tab, { url: '', pendingUrl: args.url, status: 'loading' });
    return tab;
  };
  const opened = await controller.call(A, 'chrome_open', { url: 'https://example.com/first' });
  const before = commands.length;
  assert.deepEqual((await controller.call(A, 'chrome_tabs')).tabs, [{ tab_id: opened.tab_id, origin: opened.origin, owned: true, loading: true }]);
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /page_loading:.*same tab/);
  await assert.rejects(controller.call(A, 'chrome_screenshot'), /page_loading:/);
  assert.equal(commands.length, before);
  Object.assign(pages.get(opened.tab_id), { url: 'https://example.com/first', pendingUrl: undefined, status: 'complete' });
  const snapshot = await controller.call(A, 'chrome_snapshot');
  assert.equal(snapshot.tab_id, opened.tab_id); assert.ok(snapshot.revision);
  assert.equal(created.length, 1);
});

test('a pending authorized URL grants no access to or closing of a different committed origin', async () => {
  const { controller, pages, commands, removed } = fixture(); controller.allowNewTabs = true;
  const opened = await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  const tab = pages.get(opened.tab_id);
  Object.assign(tab, { url: 'https://private.example/secret', pendingUrl: 'https://example.com', status: 'loading', title: 'private title' });
  const before = commands.length;
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /page_loading:/);
  await assert.rejects(controller.call(A, 'chrome_select_tab', { tab_id: opened.tab_id, origin: opened.origin }), /page_loading:/);
  assert.equal(commands.length, before);
  assert.equal((await controller.call(A, 'chrome_release', { close_owned: true })).closed, false);
  assert.deepEqual(removed, []);
  tab.pendingUrl = undefined;
  await assert.rejects(controller.call(A, 'chrome_select_tab', { tab_id: opened.tab_id, origin: opened.origin }), /origin_changed:/);
});

test('navigation invalidates an action revision and never replays input', async () => {
  const { controller, pages, commands } = fixture(); await controller.share(2);
  const snapshot = await controller.call(A, 'chrome_select_tab', { tab_id: 2, origin: 'https://shared.example' });
  pages.get(2).pendingUrl = 'https://shared.example/next';
  await assert.rejects(controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: snapshot.revision }), /page_loading:/);
  delete pages.get(2).pendingUrl;
  await assert.rejects(controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: snapshot.revision }), /stale_snapshot:/);
  assert.equal(commands.some(command => command.method === 'Input.dispatchMouseEvent'), false);
  const fresh = await controller.call(A, 'chrome_snapshot');
  await controller.call(A, 'chrome_action', { action: 'click', ref: 'e1', revision: fresh.revision });
  assert.equal(commands.filter(command => command.method === 'Input.dispatchMouseEvent').length, 2);
});

test('the committed CDP frame is checked before any DOM operation when tab metadata lags navigation', async () => {
  const { controller, api, commands } = fixture(); controller.allowNewTabs = true;
  await controller.call(A, 'chrome_open', { url: 'https://example.com' });
  const command = api.debugger.sendCommand;
  let frameUrl = 'about:blank';
  api.debugger.sendCommand = (target, method, args) => method === 'Page.getFrameTree'
    ? { frameTree: { frame: { id: 'frame', url: frameUrl } } } : command(target, method, args);
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /page_loading:/);
  frameUrl = 'https://private.example/secret';
  await assert.rejects(controller.call(A, 'chrome_snapshot'), /origin_changed:/);
  assert.equal(commands.some(command => command.method === 'Runtime.callFunctionOn'), false);
  frameUrl = 'https://example.com';
  assert.ok((await controller.call(A, 'chrome_snapshot')).revision);
});

test('an isolated world refuses an uncommitted or unapproved document before reading the DOM', () => {
  for (const [href, origin, expected] of [
    ['about:blank', 'null', /page_loading:/],
    ['https://private.example/secret', 'https://private.example', /origin_changed:/],
  ]) {
    const context = { location: { href, origin }, get document() { throw new Error('DOM must remain unread'); } };
    assert.throws(() => runInNewContext(`(${pageOperation.toString()})('snapshot', {expected_origin:'https://example.com'})`, context), expected);
  }
});
