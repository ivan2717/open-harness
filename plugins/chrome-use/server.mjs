import { ChromeBridge } from './bridge.mjs';
import { requestWithLaunch } from './launch-chrome.mjs';
import { tools, validate } from './schema.mjs';
import { LineDecoder } from './framing.mjs';
const args = process.argv.slice(2);
if (args.length && (args.length !== 2 || args[0] !== '--task-id')) throw new Error('Invalid Chrome server arguments.');
const bridge = new ChromeBridge({ taskId: args[1] });
const versions = ['2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05'];
let initialized = false; let closed = false; const active = new Set(); let generation = 0; let queue = Promise.resolve();
const send = message => { if (!closed) process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', ...message })}\n`); };
const shutdown = () => { closed = true; generation++; bridge.close(); process.stdin.destroy(); };
async function handle(request) {
  if (!request || request.jsonrpc !== '2.0' || typeof request.method !== 'string') { send({ id: null, error: { code: -32600, message: 'Invalid JSON-RPC request.' } }); return; }
  const { id, method, params = {} } = request;
  if (method === 'notifications/cancelled') { if (active.has(params.requestId)) { generation++; bridge.close(); } return; }
  if (id === undefined) return;
  if (!['number', 'string'].includes(typeof id) || !params || typeof params !== 'object' || Array.isArray(params)) { send({ id: null, error: { code: -32600, message: 'Invalid request.' } }); return; }
  if (active.has(id)) { send({ id, error: { code: -32600, message: 'Duplicate active request ID.' } }); return; }
  active.add(id);
  try {
    let result;
    if (method === 'initialize') { initialized = true; result = { protocolVersion: versions.includes(params.protocolVersion) ? params.protocolVersion : versions[0], capabilities: { tools: {} }, serverInfo: { name: 'openharness-chrome', version: '0.2.0' } }; }
    else if (!initialized) { send({ id, error: { code: -32002, message: 'Initialize first.' } }); return; }
    else if (method === 'ping') result = {};
    else if (method === 'tools/list') result = { tools };
    else if (method === 'tools/call') {
      try {
        validate(params.name, params.arguments ?? {});
        const current = generation;
        const operation = queue.then(() => { if (current !== generation || closed) throw new Error('Chrome operation cancelled.'); return requestWithLaunch(bridge, params.name, params.arguments ?? {}, { shouldCancel: () => closed || current !== generation }); }); queue = operation.catch(() => {});
        const { image, ...value } = await operation;
        result = { content: [{ type: 'text', text: JSON.stringify(value) }] };
        if (image) {
          if (image.mimeType !== 'image/jpeg' || typeof image.data !== 'string' || image.data.length > 6 * 1024 * 1024) throw new Error('Invalid Chrome screenshot.');
          result.content.push({ type: 'image', ...image });
        }
      } catch (error) { result = { isError: true, content: [{ type: 'text', text: JSON.stringify({ error: error.message, verified: false, effect: 'Observe before retrying actions; a disconnected or timed-out action may already have taken effect.' }) }] }; }
    } else { send({ id, error: { code: -32601, message: 'Method not found.' } }); return; }
    send({ id, result });
  } finally { active.delete(id); }
}
const decoder = new LineDecoder(message => { void handle(message).catch(shutdown); });
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { try { decoder.push(chunk); } catch { shutdown(); } });
process.stdin.once('end', shutdown); process.stdin.once('error', shutdown); process.stdout.once('error', shutdown);
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, shutdown);
