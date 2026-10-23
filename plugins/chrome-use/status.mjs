import { ChromeBridge } from './bridge.mjs';
const bridge = new ChromeBridge({ timeoutMs: 4000 });
try {
  const result = await bridge.request('chrome_status');
  process.stdout.write(`${JSON.stringify({ connected: result?.connected === true, allow_new_tabs: result?.allow_new_tabs === true, shared_tabs: Number.isInteger(result?.shared_tabs) ? result.shared_tabs : 0 })}\n`);
} catch (error) { process.stdout.write(`${JSON.stringify({ connected: false, error: error.message })}\n`); }
finally { bridge.close(); }
