// Explicit acceptance against an already connected browser. Creates only a
// disposable local fixture tab; never enumerates or shares existing user tabs.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';

for (const key of ['OPENHARNESS_CHROME_CONFIG', 'OPENHARNESS_PREVIEW_ENDPOINT', 'OPENHARNESS_PREVIEW_RUNTIME'])
  if (!process.env[key]) throw new Error(`${key} is required for installed acceptance.`);
const server = createServer((_request, response) => {
  response.setHeader('Content-Type', 'text/html; charset=utf-8');
  response.end('<!doctype html><title>浏览器画中画验收</title><body style="font:24px system-ui;padding:48px;background:#edf5e9"><h1>Open Harness 浏览器预览</h1><p>这是后台测试标签页，不会切换你正在看的页面。</p><label>测试输入 <input></label><p id="clock"></p><script>setInterval(()=>clock.textContent=new Date().toLocaleTimeString(),1000)</script></body>');
});
server.listen(0, '127.0.0.1'); await once(server, 'listening');
const target = process.env.OPENHARNESS_TEST_PLUGIN_SERVER ?? fileURLToPath(new URL('../server.mjs', import.meta.url));
const child = spawn(process.execPath, [target, '--task-id', 'browser-preview-acceptance'], { env: process.env, stdio: ['pipe', 'pipe', 'pipe'] });
let id = 0; let buffer = ''; const pending = new Map(); let opened = false;
child.stdout.setEncoding('utf8'); child.stdout.on('data', chunk => {
  buffer += chunk; let end;
  while ((end = buffer.indexOf('\n')) >= 0) {
    const result = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
    const item = pending.get(result.id); if (item) { pending.delete(result.id); clearTimeout(item.timer); item.resolve(result); }
  }
});
child.stderr.on('data', () => {});
function request(method, params = {}) {
  return new Promise((resolve, reject) => {
    const requestId = ++id;
    const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`Timed out: ${method}`)); }, 30_000);
    pending.set(requestId, { resolve, reject, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }) + '\n');
  });
}
async function call(name, args = {}) {
  const { result, error } = await request('tools/call', { name, arguments: args });
  assert(!error && !result?.isError, `Failed ${name}`);
  return JSON.parse(result.content[0].text);
}
try {
  await request('initialize', { protocolVersion: '2025-11-25' });
  const listing = await request('tools/list');
  assert(!listing.result.tools.some(tool => tool.name.includes('preview')));
  await call('chrome_open', { url: `http://127.0.0.1:${server.address().port}/` }); opened = true;
  let snapshot;
  for (let attempt = 0; attempt < 20; attempt++) {
    try { snapshot = await call('chrome_snapshot'); break; } catch { await new Promise(resolve => setTimeout(resolve, 250)); }
  }
  assert(snapshot?.revision);
  // Allow automatic preview frames to arrive, then use the *original* revision.
  await new Promise(resolve => setTimeout(resolve, 1800));
  // The clock mutates the fixture DOM. Take a fresh observation for this action.
  snapshot = await call('chrome_snapshot');
  const input = snapshot.elements.find(item => item.role === 'input');
  assert(input);
  await call('chrome_action', { action: 'fill', revision: snapshot.revision, ref: input.ref, text: '已验证后台预览' });
  process.stdout.write('READY: installed MCP opened its own local fixture; inspect the in-app preview now.\n');
  await new Promise(resolve => setTimeout(resolve, 45_000));
  await call('chrome_release', { close_owned: true }); opened = false;
  process.stdout.write('PASS: installed browser MCP fixture released; preview should disappear.\n');
} finally {
  if (opened) await call('chrome_release', { close_owned: true }).catch(() => {});
  child.stdin.end();
  await Promise.race([once(child, 'exit'), new Promise(resolve => setTimeout(resolve, 2000))]);
  if (child.exitCode === null) child.kill();
  for (const item of pending.values()) clearTimeout(item.timer);
  // Chromium may retain a speculative keep-alive connection after tab close.
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
