// This test operates only its own temporary fixture.app. It never requests TCC permissions.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { ComputerBridge } from '../bridge.mjs';
import { computerEndpoint, requireComputerEndpoint } from '../endpoint.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-computer-gui-'));
// Exercise the production LaunchServices app unless a raw fixture helper is
// explicitly configured. APP takes priority over the legacy HELPER override.
const helperPath = computerEndpoint();
const artifacts = process.env.OPENHARNESS_COMPUTER_TEST_ARTIFACTS;
const bridge = new ComputerBridge(helperPath);
let fixture;
let fixturePid;
let otherFixture;
let otherPid;
const checks = [];
const wait = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
async function until(predicate, description) {
  for (let attempt = 0; attempt < 40; attempt++) {
    try { const result = await predicate(); if (result) return result; } catch {}
    await wait(100);
  }
  throw new Error(`Timed out: ${description}`);
}
const loadState = () => JSON.parse(readFileSync(path.join(temporary, 'state.json'), 'utf8'));
const request = (method, params = {}) => bridge.request(method, params);
try {
  if (process.platform !== 'darwin') throw new Error('GUI smoke test requires macOS.');
  if (artifacts) {
    if (!path.isAbsolute(artifacts)) throw new Error('Test artifacts directory must be absolute.');
    mkdirSync(artifacts, { recursive: true });
  }
  requireComputerEndpoint(helperPath);
  const app = path.join(temporary, 'ComputerFixture.app');
  const executable = path.join(app, 'Contents/MacOS/ComputerFixture');
  mkdirSync(path.dirname(executable), { recursive: true });
  const compiled = spawnSync('xcrun', ['swiftc', '-module-cache-path', path.join(temporary, 'modules'), path.join(root, 'tests/Fixture.swift'), '-o', executable], { stdio: 'inherit' });
  if (compiled.error) throw compiled.error;
  if (compiled.status !== 0) throw new Error('Fixture compilation failed.');
  writeFileSync(path.join(app, 'Contents/Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.openharness.computer-fixture</string><key>CFBundleName</key><string>Open Harness Computer Fixture</string><key>CFBundleExecutable</key><string>ComputerFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  if (process.argv.includes('--build-only')) {
    console.log(JSON.stringify({ status: 'fixture_compiled', helper_path: helperPath, gui_operations: 0 }));
  } else {
    const status = await request('status');
    if (helperPath.endsWith('.app')) {
      assert.equal(status.permission_bundle_id, 'dev.openharness.computeruse', 'GUI acceptance must use the independently launched control app');
      checks.push('independent Computer Use app identity');
    }
    if (!status.accessibility || !status.screen_recording) {
      console.log(JSON.stringify({ status: 'blocked', helper_path: status.helper_path, reason: 'macOS system permissions are not granted; no apps were opened or operated', permissions: status, fixture_compiled: true }));
      process.exitCode = 2;
    } else {
      fixture = spawn('/usr/bin/open', ['-n', '-W', app, '--args', path.join(temporary, 'state.json')], { stdio: 'ignore' });
      const initial = await until(() => { const value = loadState(); fixturePid = value.pid; return value.ready ? value : null; }, 'fixture window ready');
      fixturePid = initial.pid;
      if (initial.foreground_bundle === 'com.apple.loginwindow') {
        throw Object.assign(new Error('The foreground application is com.apple.loginwindow; unlock the desktop before running GUI tests. No fixture actions were dispatched.'), { code: 'DESKTOP_LOCKED' });
      }
      const selection = { bundle_id: 'dev.openharness.computer-fixture', pid: initial.pid };
      await request('select_app', selection);
      await wait(500);
      let state = await request('observe');
      checks.push('native AX observation');
      const original = state.snapshot_id;
      await request('type', { snapshot_id: original, text: 'Hello Open Harness 中文🙂' });
      await until(() => loadState().text === 'Hello Open Harness 中文🙂', 'Unicode text input');
      checks.push('Unicode keyboard input');
      await assert.rejects(request('key', { snapshot_id: original, key: 'enter' }), /STALE_SNAPSHOT/);
      checks.push('consumed snapshot rejection');
      state = await request('observe');
      const button = state.elements.find(item => item.AXIdentifier === 'fixture-increment');
      assert.ok(button, 'fixture increment button is visible in AX');
      await request('click', { snapshot_id: state.snapshot_id, ref: button.ref });
      await until(() => loadState().clicks === 1, 'AX press');
      checks.push('AX press');
      state = await request('observe', { screenshot: true });
      assert.equal(state.image.mimeType, 'image/png');
      assert.equal(Buffer.from(state.image.data, 'base64').subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      if (artifacts) writeFileSync(path.join(artifacts, 'native-computer-fixture.png'), Buffer.from(state.image.data, 'base64'));
      checks.push('window PNG screenshot');
      const screenshotButton = state.elements.find(item => item.AXIdentifier === 'fixture-increment').bounds;
      await request('click', { snapshot_id: state.snapshot_id, x: screenshotButton.x + screenshotButton.width / 2, y: screenshotButton.y + screenshotButton.height / 2 });
      await until(() => loadState().clicks === 2, 'coordinate click');
      checks.push('coordinate click');
      // Changing the fixture out-of-band demonstrates stale tree rejection.
      state = await request('observe');
      const secondary = new ComputerBridge(helperPath);
      try {
        await secondary.request('select_app', selection);
        const fresh = await secondary.request('observe', {});
        const resize = fresh.elements.find(item => item.AXIdentifier === 'fixture-resize');
        await secondary.request('click', { snapshot_id: fresh.snapshot_id, ref: resize.ref });
        await wait(150);
      } finally { secondary.stop(); }
      await assert.rejects(request('key', { snapshot_id: state.snapshot_id, key: 'tab' }), /STATE_CHANGED/);
      checks.push('changed window rejection');
      state = await request('observe', { screenshot: true });
      const scroll = state.elements.find(item => item.AXIdentifier === 'fixture-scroll').bounds;
      const priorScroll = loadState().scroll_y;
      await request('scroll', { snapshot_id: state.snapshot_id, x: Math.max(30, scroll.x + 50), y: state.window.bounds.height - 100, delta_y: -240 });
      await until(() => loadState().scroll_y !== priorScroll, 'scroll changed fixture content offset');
      checks.push('scroll outcome');
      state = await request('observe', { screenshot: true });
      if (artifacts) {
        writeFileSync(path.join(artifacts, 'native-computer-fixture.png'), Buffer.from(state.image.data, 'base64'));
        writeFileSync(path.join(artifacts, 'native-computer-fixture-state.json'), JSON.stringify(loadState(), null, 2));
      }
      // A second instance is also our own fixture, never a user application.
      const otherStatePath = path.join(temporary, 'other-state.json');
      otherFixture = spawn('/usr/bin/open', ['-n', '-W', app, '--args', otherStatePath], { stdio: 'ignore' });
      const otherState = await until(() => { const value = JSON.parse(readFileSync(otherStatePath, 'utf8')); otherPid = value.pid; return value.ready ? value : null; }, 'second fixture window ready');
      otherPid = otherState.pid;
      assert.notEqual(otherPid, fixturePid);
      const otherBridge = new ComputerBridge(helperPath);
      try {
        await otherBridge.request('select_app', { bundle_id: selection.bundle_id, pid: otherPid });
        await wait(500);
        await assert.rejects(request('type', { snapshot_id: state.snapshot_id, text: 'must not escape' }), /FOCUS_CHANGED/);
        assert.equal(loadState().text, 'Hello Open Harness 中文🙂');
        assert.equal(JSON.parse(readFileSync(otherStatePath, 'utf8')).text, '');
      } finally { otherBridge.stop(); }
      checks.push('focus loss rejects input without modifying either fixture');
      console.log(JSON.stringify({ status: 'passed', helper_path: status.helper_path, checks, actual_fixture: loadState() }));
    }
  }
} catch (error) {
  let locked = error?.code === 'DESKTOP_LOCKED';
  try { locked ||= loadState().foreground_bundle === 'com.apple.loginwindow'; } catch {}
  console.error(JSON.stringify({ status: locked ? 'blocked' : 'failed', helper_path: helperPath, checks, ...(locked ? { reason: 'The desktop is locked (com.apple.loginwindow is foreground). Unlock it and rerun; no success is claimed.' } : {}), error: error instanceof Error ? error.stack : String(error) }));
  if (artifacts && fixture) {
    try { writeFileSync(path.join(artifacts, 'native-computer-fixture-state.json'), JSON.stringify(loadState(), null, 2)); } catch {}
    try {
      const { image, ...state } = await request('observe', { screenshot: true });
      writeFileSync(path.join(artifacts, 'native-computer-failure.json'), JSON.stringify(state, null, 2));
      if (image) writeFileSync(path.join(artifacts, 'native-computer-failure.png'), Buffer.from(image.data, 'base64'));
    } catch {}
  }
  process.exitCode = locked ? 2 : 1;
} finally {
  bridge.stop();
  if (otherPid) { try { process.kill(otherPid, 'SIGTERM'); } catch {} }
  if (fixturePid) { try { process.kill(fixturePid, 'SIGTERM'); } catch {} }
  if (otherFixture) { otherFixture.kill('SIGTERM'); }
  if (fixture) { fixture.kill('SIGTERM'); await wait(100); if (fixture.exitCode === null) fixture.kill('SIGKILL'); }
  rmSync(temporary, { recursive: true, force: true });
}
