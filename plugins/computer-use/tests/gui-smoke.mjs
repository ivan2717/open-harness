// Real GUI acceptance uses only these two temporary applications. It never
// requests permissions or sends global keyboard/mouse events to the desktop.
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { ComputerBridge } from '../bridge.mjs';
import { computerEndpoint, requireComputerEndpoint } from '../endpoint.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-computer-gui-'));
const helperPath = computerEndpoint();
const artifacts = process.env.OPENHARNESS_COMPUTER_TEST_ARTIFACTS;
const bridge = new ComputerBridge(helperPath);
const fixtures = [];
const checks = [];
let backgroundActivationCount;
const wait = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
async function until(predicate, description) {
  for (let attempt = 0; attempt < 80; attempt++) {
    try { const result = await predicate(); if (result) return result; } catch {}
    await wait(100);
  }
  throw new Error(`Timed out: ${description}`);
}
const load = name => JSON.parse(readFileSync(path.join(temporary, `${name}.json`), 'utf8'));
const request = (method, params = {}) => bridge.request(method, params);
async function command(name, command) {
  const id = randomUUID();
  writeFileSync(path.join(temporary, `${name}.json.command.json`), JSON.stringify({ id, command }));
  await until(() => load(name).last_test_command === id, `${name} applied ${command}`);
}
const node = (state, id) => {
  const item = state.elements.find(item => item.AXIdentifier === id);
  assert.ok(item, `AX element ${id} exists`);
  return item;
};
const center = item => ({ x: item.bounds.x + item.bounds.width / 2, y: item.bounds.y + item.bounds.height / 2 });
async function act(method, state, params) {
  const result = await request(method, { snapshot_id: state.snapshot_id, ...params });
  assert.equal(result.dispatched, true, `${method} explicitly reports dispatch`);
  assert.equal(typeof result.verified, 'boolean', `${method} explicitly reports verification`);
  return result;
}
function assertForegroundB() {
  assert.equal(load('B').active, true, 'foreground fixture B stays active');
  assert.equal(load('A').active, false, 'background fixture A stays inactive');
  assert.equal(load('B').foreground_bundle, 'dev.openharness.computer-fixture-b');
  assert.equal(load('A').activation_count, backgroundActivationCount, 'A never briefly activates between assertions');
}
function eventCounts(name) {
  const value = load(name);
  return { key: value.key_events, pointer: value.pointer_events.length, text: value.text, clicks: value.clicks, second: value.second_clicks };
}
async function rejectedWithoutInput(method, state, params, pattern) {
  const beforeA = eventCounts('A');
  const beforeB = eventCounts('B');
  await assert.rejects(request(method, { snapshot_id: state.snapshot_id, ...params }), pattern);
  await wait(100);
  assert.deepEqual(eventCounts('A'), beforeA, 'rejected input never reaches A');
  assert.deepEqual(eventCounts('B'), beforeB, 'rejected input never reaches B');
  assertForegroundB();
}
function assertPointerButtonsReleased(requireTextTracking = false) {
  // NSEvent.EventType raw values; assert each observed button's event stream,
  // including the drag sequence, finishes with its matching release.
  const value = load('A');
  const events = value.pointer_events;
  if (requireTextTracking) assert.ok(value.text_mouse_tracking_started > 0, 'standard text-field tracking was exercised');
  assert.equal(value.text_mouse_tracking_finished, value.text_mouse_tracking_started, 'every text-field mouseDown tracking loop returned');
  assert.equal(value.text_mouse_tracking.length, value.text_mouse_tracking_started);
  const cellConsumedReleases = new Set();
  for (const tracking of value.text_mouse_tracking) {
    assert.equal(tracking.finished, true, 'text-field tracking finished');
    const index = tracking.down_event_index;
    assert.ok(Number.isInteger(index) && index >= 0 && index < events.length, 'tracking identifies a recorded down event');
    assert.equal(events[index].type, 1);
    for (const key of ['window', 'event_number', 'timestamp', 'x', 'y']) assert.equal(events[index][key], tracking[key], `tracking matches down event ${key}`);
    assert.ok(Number.isInteger(tracking.end_event_index) && tracking.end_event_index > index && tracking.end_event_index <= events.length);
    const duringTracking = events.slice(index + 1, tracking.end_event_index);
    // NSCell's synchronous tracking can dequeue mouseUp without redispatching
    // through NSApplication's local monitor. Exclude only the exact text-field
    // down whose standard super.mouseDown returned with no observed release.
    const nextLeftBoundary = events.slice(index + 1).find(event => event.type === 1 || event.type === 2);
    if (nextLeftBoundary?.type !== 2) {
      assert.ok(!duringTracking.some(event => event.type === 1), 'tracking exception cannot hide another left press');
      assert.ok(!cellConsumedReleases.has(index), 'tracking record is unique');
      cellConsumedReleases.add(index);
    }
  }
  const buttons = [
    { name: 'left', down: 1, up: 2, dragged: 6 },
    { name: 'right', down: 3, up: 4, dragged: 7 },
    { name: 'middle', down: 25, up: 26, dragged: 27 },
  ];
  for (const button of buttons) {
    let held = false;
    let downs = 0;
    let ups = 0;
    let drags = 0;
    for (const [index, event] of events.entries()) {
      if (button.name === 'left' && cellConsumedReleases.has(index)) continue;
      if (event.type === button.down) {
        assert.equal(held, false, `${button.name} down follows its previous release`);
        held = true;
        downs++;
      } else if (event.type === button.up) {
        assert.equal(held, true, `${button.name} up has a matching down`);
        held = false;
        ups++;
      } else if (event.type === button.dragged) {
        assert.equal(held, true, `${button.name} drag occurs while its button is held`);
        drags++;
      }
    }
    assert.ok(downs > 0, `${button.name} dispatch was exercised`);
    assert.equal(ups, downs, `${button.name} down/up count is balanced`);
    assert.equal(held, false, `${button.name} is released at the end`);
    if (button.name === 'left') assert.ok(drags > 0, 'drag sequence was exercised and released');
  }
  assert.equal(load('B').pointer_events.length, 0, 'B received no pointer events');
}
function buildFixtures() {
  const binary = path.join(temporary, 'ComputerFixture');
  const compiled = spawnSync('xcrun', ['swiftc', '-module-cache-path', path.join(temporary, 'modules'), path.join(root, 'tests/Fixture.swift'), '-o', binary], { stdio: 'inherit' });
  if (compiled.error) throw compiled.error;
  if (compiled.status !== 0) throw new Error('Fixture compilation failed.');
  return Object.fromEntries(['A', 'B'].map(name => {
    const app = path.join(temporary, `ComputerFixture${name}.app`);
    const executable = path.join(app, 'Contents/MacOS/ComputerFixture');
    mkdirSync(path.dirname(executable), { recursive: true });
    copyFileSync(binary, executable);
    writeFileSync(path.join(app, 'Contents/Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.openharness.computer-fixture-${name.toLowerCase()}</string><key>CFBundleName</key><string>Open Harness Fixture ${name}</string><key>CFBundleExecutable</key><string>ComputerFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
    return [name, app];
  }));
}
async function launch(name, app) {
  const child = spawn('/usr/bin/open', ['-n', '-W', app, '--args', path.join(temporary, `${name}.json`)], { stdio: 'ignore' });
  const fixture = { name, child, pid: undefined };
  fixtures.push(fixture);
  const initial = await until(() => { const value = load(name); fixture.pid = value.pid; return value.ready ? value : null; }, `fixture ${name} ready`);
  fixture.pid = initial.pid;
  return initial;
}
try {
  if (process.platform !== 'darwin') throw new Error('GUI smoke test requires macOS.');
  if (artifacts) {
    if (!path.isAbsolute(artifacts)) throw new Error('Test artifacts directory must be absolute.');
    mkdirSync(artifacts, { recursive: true });
  }
  const apps = buildFixtures();
  if (process.argv.includes('--build-only')) {
    console.log(JSON.stringify({ status: 'fixtures_compiled', helper_path: helperPath, fixture_count: 2, gui_operations: 0 }));
  } else {
    requireComputerEndpoint(helperPath);
    const status = await request('status');
    if (helperPath.endsWith('.app')) {
      assert.equal(status.permission_bundle_id, 'dev.openharness.computeruse');
      checks.push('independent Computer Use app identity');
    }
    if (!status.accessibility || !status.screen_recording) {
      console.log(JSON.stringify({ status: 'blocked', helper_path: status.helper_path, reason: 'macOS system permissions are not granted; no fixtures were opened or operated', permissions: status, fixtures_compiled: true }));
      process.exitCode = 2;
    } else {
      const initialA = await launch('A', apps.A);
      const initialB = await launch('B', apps.B);
      assert.notEqual(initialA.pid, initialB.pid);
      await command('B', 'activate');
      await until(() => load('B').active && !load('A').active, 'B frontmost over A');
      const selection = { bundle_id: initialA.bundle_id, pid: initialA.pid, window_id: initialA.window_id };
      const activationsBeforeSelect = load('A').activation_count;
      backgroundActivationCount = activationsBeforeSelect;
      await request('select_app', selection);
      await wait(150);
      assertForegroundB();
      assert.equal(load('A').activation_count, activationsBeforeSelect, 'select never activates A');
      checks.push('select preserves foreground B while binding background A');
      const listing = await request('windows');
      assert.ok(listing.windows.some(item => item.id === initialA.window_id), 'windows enumerates exact primary ID');
      let state = await request('observe');
      assert.equal(state.window.id, initialA.window_id);
      checks.push('window inventory and stable AX observation');

      // The tree and pixels change continuously. Validation must concern the
      // action's target, not an unrelated animation elsewhere in the window.
      const animationAtObserve = load('A').animation_frame;
      const button = node(state, 'fixture-increment');
      await until(() => load('A').animation_frame > animationAtObserve + 2, 'background animation advanced');
      await act('click', state, { ref: button.ref });
      await until(() => load('A').clicks === 1, 'background AX press');
      assertForegroundB();
      checks.push('AX press tolerates unrelated animation');

      state = await request('observe');
      await act('type', state, { text: 'Hello Open Harness 中文🙂' });
      await until(() => load('A').text === 'Hello Open Harness 中文🙂', 'background Unicode input');
      await rejectedWithoutInput('key', state, { key: 'enter' }, /STALE_SNAPSHOT/);
      checks.push('background Unicode input and consumed snapshot rejection');



      state = await request('observe');
      await act('set_value', state, { ref: node(state, 'fixture-input').ref, value: 'AX assigned value' });
      await until(() => load('A').text === 'AX assigned value', 'AX set value outcome');
      state = await request('observe');
      await rejectedWithoutInput('set_value', state, { ref: node(state, 'fixture-secret').ref, value: 'must not write' }, /SECURE|PASSWORD|SECRET|UNSUPPORTED/i);
      assert.equal(load('A').secret_length, 0);
      await command('A', 'focus_secret');
      state = await request('observe');
      await rejectedWithoutInput('type', state, { text: 'must not write' }, /SECURE|PASSWORD|SECRET|FOCUS_UNAVAILABLE/i);
      state = await request('observe');
      await rejectedWithoutInput('key', state, { key: 'meta+a' }, /SECURE|PASSWORD|SECRET|FOCUS_UNAVAILABLE/i);
      assert.equal(load('A').secret_length, 0);
      await command('A', 'focus_input');
      checks.push('AX value assignment and password field refusal for value, text and keys');

      state = await request('observe');
      const actionButton = node(state, 'fixture-increment');
      assert.ok(actionButton.actions.includes('AXPress'), 'AXPress is advertised');
      await act('action', state, { ref: actionButton.ref, action: 'AXPress' });
      await until(() => load('A').clicks === 2, 'advertised AX action outcome');
      state = await request('observe');
      await rejectedWithoutInput('action', state, { ref: node(state, 'fixture-increment').ref, action: 'AXUnadvertisedFixtureAction' }, /ACTION|UNSUPPORTED/i);
      checks.push('advertised AX action only');

      state = await request('observe', { screenshot: true });
      const beforeButtonMove = center(node(state, 'fixture-increment'));
      await command('A', 'move_button');
      await rejectedWithoutInput('click', state, beforeButtonMove, /STATE_CHANGED|AX_UNAVAILABLE|TARGET_UNAVAILABLE/);
      await command('A', 'restore_button');
      state = await request('observe', { screenshot: true });
      const beforeButtonReplacement = center(node(state, 'fixture-increment'));
      await command('A', 'replace_button');
      await rejectedWithoutInput('click', state, beforeButtonReplacement, /STATE_CHANGED|AX_UNAVAILABLE|TARGET_UNAVAILABLE/);
      checks.push('coordinate targets reject moved and replaced controls without dispatch');

      state = await request('observe', { screenshot: true });
      assert.equal(state.image.mimeType, 'image/png');
      assert.equal(Buffer.from(state.image.data, 'base64').subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      if (artifacts) writeFileSync(path.join(artifacts, 'native-computer-fixture.png'), Buffer.from(state.image.data, 'base64'));
      const screenshotPoint = center(node(state, 'fixture-increment'));
      const animated = load('A').animation_frame;
      await until(() => load('A').animation_frame > animated + 2, 'pixels changed after screenshot');
      await act('click', state, screenshotPoint);
      await until(() => load('A').clicks === 3, 'fully covered background coordinate click');
      assertForegroundB();
      checks.push('covered-window PNG and coordinate click tolerate animation');

      state = await request('observe', { screenshot: true });
      const movedPoint = center(node(state, 'fixture-increment'));
      await command('A', 'move');
      await act('click', state, movedPoint);
      await until(() => load('A').clicks === 4, 'window-local click after translation');
      checks.push('translation preserves window-local coordinates');

      for (const [buttonName, key] of [['right', 'right_clicks'], ['middle', 'middle_clicks']]) {
        state = await request('observe', { screenshot: true });
        const prior = load('A')[key];
        await act('click', state, { ...center(node(state, 'fixture-pad')), button: buttonName });
        await until(() => load('A')[key] > prior, `${buttonName} click reached A`);
      }
      state = await request('observe', { screenshot: true });
      await act('click', state, { ...center(node(state, 'fixture-pad')), click_count: 3, modifiers: ['shift'] });
      await until(() => load('A').last_click_count === 3, 'triple click count reached fixture');
      assert.ok(load('A').last_modifiers & (1 << 17), 'shift modifier delivered');
      checks.push('right/middle click, click count and modifier delivery');

      state = await request('observe', { screenshot: true });
      const pad = center(node(state, 'fixture-pad'));
      await act('drag', state, { from_x: pad.x - 80, from_y: pad.y, to_x: pad.x + 80, to_y: pad.y });
      await until(() => {
        const value = load('A');
        return value.drags > 0 && value.drag_start && value.drag_end && Math.abs(value.drag_end.x - value.drag_start.x - 160) < 2;
      }, 'drag delivered the requested 160-point displacement');
      assert.ok(Math.abs(load('A').drag_end.y - load('A').drag_start.y) < 2, 'horizontal drag preserves y');
      state = await request('observe', { screenshot: true });
      const priorScroll = load('A').scroll_y;
      await act('scroll', state, { x: 100, y: state.window.bounds.height - 100, delta_y: -240 });
      await until(() => load('A').scroll_y !== priorScroll, 'background scroll changed content offset');
      assert.equal(load('B').pointer_events.length, 0, 'all pointer events remained in A');
      assertForegroundB();
      checks.push('background drag and scroll outcomes');

      state = await request('observe', { screenshot: true });
      const cancellationPad = center(node(state, 'fixture-pad'));
      const priorEvents = load('A').pointer_events.length;
      const cancellation = new AbortController();
      const cancelledDrag = bridge.request('drag', { snapshot_id: state.snapshot_id, from_x: cancellationPad.x - 80, from_y: cancellationPad.y, to_x: cancellationPad.x + 80, to_y: cancellationPad.y }, cancellation.signal).then(result => ({ result }), error => ({ error }));
      let cancelledWhileHeld = false;
      for (let attempt = 0; attempt < 500; attempt++) {
        const events = load('A').pointer_events.slice(priorEvents);
        if (events.some(event => event.type === 1) && !events.some(event => event.type === 2)) {
          cancellation.abort(); cancelledWhileHeld = true; break;
        }
        await wait(1);
      }
      assert.equal(cancelledWhileHeld, true, 'cancellation caught a held target mouse button');
      assert.match(String((await cancelledDrag).error), /cancelled/i);
      await until(() => load('A').pointer_events.slice(priorEvents).some(event => event.type === 2), 'cancelled drag released its target button');
      assertPointerButtonsReleased();
      assertForegroundB();
      await request('select_app', selection);
      checks.push('cancelling a held background drag releases its button and ends the session');


      await command('A', 'focus_input');
      state = await request('observe');
      await act('key', state, { key: 'meta+a' });
      state = await request('observe');
      await act('type', state, { text: 'replaced by combo' });
      await until(() => load('A').text === 'replaced by combo', 'background Command+A replaced selection');
      assert.equal(load('B').text, '');
      assertForegroundB();
      checks.push('background combination key targets A only');

      state = await request('observe');
      await act('set_value', state, { ref: node(state, 'fixture-input').ref, value: '' });
      await command('A', 'focus_pad');
      state = await request('observe', { screenshot: true });
      await act('click', state, center(node(state, 'fixture-input')));
      state = await until(async () => {
        const observed = await request('observe');
        return node(observed, 'fixture-input').AXFocused ? observed : null;
      }, 'background field acquired focus after queued click');
      await act('type', state, { text: 'Coordinate-focused text' });
      await until(() => load('A').text === 'Coordinate-focused text', 'coordinate click focused the standard background text field');
      assert.equal(load('B').text, '');
      assertForegroundB();
      checks.push('coordinate click on standard background text field focuses subsequent typing');

      // B types into its own responder while A is controlled. This does not
      // invoke a global event tap or send anything to a user application.
      await command('A', 'focus_input');
      await command('B', 'foreground_input');
      state = await request('observe');
      await act('key', state, { key: 'meta+a' });
      state = await request('observe');
      await act('type', state, { text: 'A remains isolated' });
      await until(() => load('B').foreground_input_remaining === 0, 'foreground responder input completed');
      assert.equal(load('A').text, 'A remains isolated');
      assert.equal(load('B').text, 'u'.repeat(60));
      assert.equal(load('B').foreground_input_count, 60);
      assert.equal(load('B').foreground_input_interrupted, false);
      assertForegroundB();
      checks.push('concurrent foreground responder typing remains isolated');

      state = await request('observe');
      await rejectedWithoutInput('click', state, { x: 100, y: 100 }, /SCREENSHOT_REQUIRED/);
      state = await request('observe', { screenshot: true });
      await rejectedWithoutInput('drag', state, { from_x: 50, from_y: 50, to_x: state.window.bounds.width + 1, to_y: 50 }, /INVALID_COORDINATES/);
      checks.push('screenshot and coordinate boundaries reject without dispatch');

      state = await request('observe');
      await command('A', 'resize');
      await rejectedWithoutInput('key', state, { key: 'tab' }, /STATE_CHANGED|WINDOW_CHANGED/);
      checks.push('resized window invalidates snapshot without input');

      await command('A', 'second_window');
      const twoWindows = await until(async () => {
        const listing = await request('windows');
        return listing.windows.some(item => item.id === load('A').second_window_id) ? listing : null;
      }, 'secondary window registered with AX and WindowServer');
      assert.ok(twoWindows.windows.some(item => item.id === load('A').second_window_id));
      const stillPrimary = await request('observe');
      assert.equal(stillPrimary.window.id, initialA.window_id, 'observe remains bound despite app focus switching');
      await act('click', stillPrimary, { ref: node(stillPrimary, 'fixture-increment').ref });
      await until(() => load('A').clicks === 5, 'bound primary receives AX press');
      assert.equal(load('A').second_clicks, 0);
      assertForegroundB();
      checks.push('same-app focused-window change preserves bound window identity');

      state = await request('observe', { screenshot: true });
      await command('A', 'close_primary');
      await rejectedWithoutInput('click', state, { x: 100, y: 100 }, /WINDOW|STATE_CHANGED/);
      await assert.rejects(request('observe'), /WINDOW|STATE_CHANGED/);
      await until(async () => {
        const listing = await request('windows');
        return listing.windows.some(item => item.id === load('A').second_window_id) && !listing.windows.some(item => item.id === initialA.window_id);
      }, 'closed primary disappeared while secondary remains available');
      await request('select_app', { ...selection, window_id: load('A').second_window_id });
      state = await request('observe');
      await act('click', state, { ref: node(state, 'fixture-secondary-increment').ref });
      await until(() => load('A').second_clicks === 1, 'explicitly selected secondary receives action');
      assertForegroundB();
      checks.push('destroyed window refuses input and explicit secondary selection succeeds');
      assertPointerButtonsReleased(true);
      checks.push('left/right/middle presses balance releases and drag finishes released');
      if (artifacts) {
        for (const name of ['A', 'B']) writeFileSync(path.join(artifacts, `native-computer-fixture-${name}.json`), JSON.stringify(load(name), null, 2));
      }
      console.log(JSON.stringify({ status: 'passed', helper_path: status.helper_path, checks, fixture_a: load('A'), fixture_b: load('B') }));
    }
  }
} catch (error) {
  const locked = error?.code === 'DESKTOP_LOCKED' || /DESKTOP_LOCKED|SESSION_LOCKED/.test(String(error));
  console.error(JSON.stringify({ status: locked ? 'blocked' : 'failed', helper_path: helperPath, checks, ...(locked ? { reason: 'Desktop is locked; no success is claimed.' } : {}), error: error instanceof Error ? error.stack : String(error) }));
  if (artifacts && fixtures.length) {
    for (const name of ['A', 'B']) {
      try { writeFileSync(path.join(artifacts, `native-computer-fixture-${name}.json`), JSON.stringify(load(name), null, 2)); } catch {}
    }
    try {
      const { image, ...state } = await request('observe', { screenshot: true });
      writeFileSync(path.join(artifacts, 'native-computer-failure.json'), JSON.stringify(state, null, 2));
      if (image) writeFileSync(path.join(artifacts, 'native-computer-failure.png'), Buffer.from(image.data, 'base64'));
    } catch {}
  }
  process.exitCode = locked ? 2 : 1;
} finally {
  bridge.stop();
  for (const { pid, child } of fixtures.reverse()) {
    if (pid) { try { process.kill(pid, 'SIGTERM'); } catch {} }
    child.kill('SIGTERM');
  }
  if (fixtures.length) await wait(100);
  for (const { child } of fixtures) if (child.exitCode === null) child.kill('SIGKILL');
  rmSync(temporary, { recursive: true, force: true });
}
