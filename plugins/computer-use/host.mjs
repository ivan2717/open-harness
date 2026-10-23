// Host-only permission bridge. It never exposes permission prompting as MCP tools.
import { ComputerBridge } from './bridge.mjs';
import { computerEndpoint, requireComputerEndpoint } from './endpoint.mjs';

const [, , flag, method, ...extra] = process.argv;
const allowed = ['status', 'request_accessibility', 'request_screen_recording'];
let bridge;
process.on('SIGTERM', () => { bridge?.stop(); process.exit(0); });
process.on('SIGINT', () => { bridge?.stop(); process.exit(0); });
try {
  if (flag !== '--request' || !allowed.includes(method) || extra.length) throw new Error('Invalid host permission request.');
  if (process.platform !== 'darwin') throw new Error('Computer Use requires macOS.');
  const endpoint = computerEndpoint();
  requireComputerEndpoint(endpoint);
  bridge = new ComputerBridge(endpoint, [], { timeoutMs: method === 'status' ? 15_000 : 60_000 });
  const result = await bridge.request(method, {});
  process.stdout.write(`${JSON.stringify({ id: 1, result })}\n`);
} catch (error) {
  process.stdout.write(`${JSON.stringify({ id: 1, error: { code: 'COMPUTER_HOST_ERROR', message: error.message } })}\n`);
} finally { bridge?.stop(); }
