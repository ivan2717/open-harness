import { ComputerBridge } from './bridge.mjs';
import { tools, validate } from './tools.mjs';
import { errorMetadata, metadataText } from './output.mjs';
import { computerEndpoint, requireComputerEndpoint, requireComputerPlatform } from './endpoint.mjs';

const helperPath = computerEndpoint();
const bridge = new ComputerBridge(helperPath);
const active = new Map();
const send = message => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', ...message })}\n`);
const text = result => ({ type: 'text', text: metadataText(result) });
let initialized = false;
let buffer = '';
const close = () => { for (const controller of active.values()) controller.abort(); bridge.stop(); };
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => {
  buffer += chunk;
  if (buffer.length > 64 * 1024) { close(); process.exitCode = 1; process.stdin.destroy(); return; }
  let end;
  while ((end = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
    if (!line.trim()) continue;
    let request;
    try { request = JSON.parse(line); } catch { send({ id: null, error: { code: -32700, message: 'Invalid JSON.' } }); continue; }
    void handle(request);
  }
});
process.stdin.once('end', close);
process.on('SIGTERM', () => { close(); process.exit(0); });
process.on('SIGINT', () => { close(); process.exit(0); });

async function handle(request) {
  if (!request || typeof request !== 'object' || request.jsonrpc !== '2.0' || typeof request.method !== 'string') {
    send({ id: null, error: { code: -32600, message: 'Invalid JSON-RPC request.' } }); return;
  }
  if (request.method === 'notifications/cancelled') {
    active.get(request.params?.requestId)?.abort(); return;
  }
  if (!Object.hasOwn(request, 'id')) return;
  const { id, method, params = {} } = request;
  if ((typeof id !== 'string' && typeof id !== 'number') || !params || typeof params !== 'object' || Array.isArray(params)) {
    send({ id: id ?? null, error: { code: -32602, message: 'Invalid request ID or params.' } }); return;
  }
  if (active.has(id)) { send({ id, error: { code: -32600, message: 'Duplicate active request ID.' } }); return; }
  if (method === 'initialize') {
    initialized = true;
    const supported = ['2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05'];
    send({ id, result: { protocolVersion: supported.includes(params.protocolVersion) ? params.protocolVersion : '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'openharness-computer-use', version: '0.5.0' } } }); return;
  }
  if (method === 'ping') { send({ id, result: {} }); return; }
  if (!initialized) { send({ id, error: { code: -32000, message: 'Initialize first.' } }); return; }
  if (method === 'tools/list') { send({ id, result: { tools } }); return; }
  if (method !== 'tools/call') { send({ id, error: { code: -32601, message: 'Method not found.' } }); return; }
  const controller = new AbortController();
  active.set(id, controller);
  let requested = false;
  try {
    const args = params.arguments ?? {};
    validate(params.name, args);
    if (params.name === 'computer_release') {
      bridge.stop();
      send({ id, result: { content: [text({ released: true })] } }); return;
    }
    requireComputerPlatform();
    requireComputerEndpoint(helperPath);
    requested = true;
    const result = await bridge.request(params.name.replace('computer_', ''), args, controller.signal);
    const content = [];
    const { image, ...metadata } = result;
    content.push(text(metadata));
    if (image) {
      if (image.mimeType !== 'image/png' || typeof image.data !== 'string' || image.data.length > 6 * 1024 * 1024) throw new Error('Invalid screenshot returned by helper.');
      content.push({ type: 'image', mimeType: image.mimeType, data: image.data });
    }
    send({ id, result: { content } });
  } catch (error) {
    send({ id, result: { isError: true, content: [text(errorMetadata(error, requested))] } });
  } finally { active.delete(id); }
}
