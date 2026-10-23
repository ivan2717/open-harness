import test from 'node:test';
import assert from 'node:assert/strict';
import { ComputerBridge } from '../bridge.mjs';
import { requireComputerEndpoint } from '../endpoint.mjs';

// Explicit opt-in: the ordinary unit suite never launches a real GUI component.
// Even when enabled this test calls status only, never permission prompting,
// observation, selection, input or capture of any user application.
const app = process.env.OPENHARNESS_COMPUTER_TEST_APP;
test('LaunchServices starts the independent identity and channel closure exits it', { skip: process.platform !== 'darwin' || !app, timeout: 20_000 }, async t => {
  assert.ok(app.endsWith('.app'));
  requireComputerEndpoint(app);
  const bridge = new ComputerBridge(app);
  t.after(() => bridge.stop());
  const result = await bridge.request('status', {});
  assert.equal(result.permission_app_name, 'Open Harness Computer Use');
  assert.equal(result.permission_bundle_id, 'dev.openharness.computeruse');
  assert.equal(result.permissions_requested, false);
  assert.equal(result.selected_app, null);
  assert.equal(typeof result.accessibility, 'boolean');
  assert.equal(typeof result.screen_recording, 'boolean');
  const pid = bridge.child.pid;
  assert.ok(pid > 0);
  bridge.stop();
  let alive = true;
  for (let i = 0; i < 100; i++) {
    try { process.kill(pid, 0); } catch (error) { if (error.code === 'ESRCH') { alive = false; break; } throw error; }
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.equal(alive, false, 'the independently launched app must exit when its host channel closes');
});
