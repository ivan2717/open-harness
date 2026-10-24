import test from 'node:test';
import assert from 'node:assert/strict';
import { metadataText, MAX_METADATA_BYTES } from '../output.mjs';

test('large multilingual AX observations preserve complete JSON, action refs and coordinate metadata', () => {
  const label = '文档🙂\\"\n'.repeat(400);
  const input = {
    snapshot_id: 'fixture-snapshot',
    selected_app: { bundle_id: 'dev.openharness.fixture', pid: 123 },
    window: { id: 456, bounds: { x: 20, y: 40, width: 800, height: 600 } },
    coordinate_space: 'window-local points; origin top left',
    screenshot: { width: 800, height: 600, window_width_points: 800, window_height_points: 600 },
    elements: Array.from({ length: 250 }, (_, i) => ({
      ref: `e${i}`, role: i >= 247 ? 'AXButton' : 'AXStaticText', depth: 3,
      AXTitle: label, AXDescription: label, AXValue: label,
      AXFocused: i === 249,
      bounds: { x: 10, y: i, width: 60, height: 20 },
    })),
    truncated: false,
  };
  const text = metadataText(input);
  assert.ok(Buffer.byteLength(JSON.stringify({ content: [{ type: 'text', text }] })) <= MAX_METADATA_BYTES);
  const result = JSON.parse(text);
  assert.equal(result.snapshot_id, input.snapshot_id);
  for (const key of ['selected_app', 'window', 'coordinate_space', 'screenshot']) assert.deepEqual(result[key], input[key]);
  assert.equal(result.truncated, true);
  assert.deepEqual(result.elements.filter(item => item.role === 'AXButton').map(item => item.ref), ['e247', 'e248', 'e249']);
  assert.ok(result.elements.every(item => [item.AXTitle, item.AXDescription, item.AXValue].every(text => text.isWellFormed())));
  assert.equal(input.elements.length, 250, 'serialization must not alter native snapshot state');
  assert.equal(input.elements[0].AXTitle, label);
});

test('small observations are unchanged and large application catalogs keep exact identities', () => {
  const small = { snapshot_id: 'a', elements: [{ ref: 'e0', AXTitle: '中文🙂' }] };
  assert.deepEqual(JSON.parse(metadataText(small)), small);
  const applications = Array.from({ length: 800 }, (_, i) => ({ name: '应用🙂'.repeat(400), bundle_id: `dev.fixture.${i}`, pid: i + 1 }));
  const text = metadataText({ applications });
  assert.ok(Buffer.byteLength(JSON.stringify({ content: [{ type: 'text', text }] })) <= MAX_METADATA_BYTES);
  const output = JSON.parse(text);
  assert.equal(output.truncated, true);
  assert.ok(output.applications.length > 0);
  for (const app of output.applications) assert.equal(app.bundle_id, `dev.fixture.${app.pid - 1}`);
});

test('large Windows UI Automation observations keep focused edits and exact Windows identities', () => {
  const long = 'Windows 中文🙂\\"\n'.repeat(400);
  const source = {
    snapshot_id: 'windows-snapshot',
    selected_app: { bundle_id: `win32:${'a'.repeat(64)}`, pid: 123 },
    coordinate_space: 'window-local physical pixels; origin top left',
    screenshot: { width: 800, height: 600, window_width_points: 1600, window_height_points: 1200 },
    elements: Array.from({ length: 250 }, (_, index) => ({
      ref: `e${index}`, role: index === 249 ? 'Edit' : 'Text',
      name: long, automation_id: long, value: long, help_text: long,
      focused: index === 249, bounds: { x: index, y: 20, width: 80, height: 30 },
    })),
  };
  const text = metadataText(source);
  const result = JSON.parse(text);
  assert.ok(Buffer.byteLength(JSON.stringify({ content: [{ type: 'text', text }] })) <= MAX_METADATA_BYTES);
  assert.equal(result.snapshot_id, source.snapshot_id);
  assert.deepEqual(result.selected_app, source.selected_app);
  assert.deepEqual(result.screenshot, source.screenshot);
  assert.ok(result.elements.some(element => element.ref === 'e249' && element.focused));
  assert.ok(result.elements.every(element => [element.name, element.automation_id, element.value].every(value => value.isWellFormed())));
  assert.equal(source.elements[249].value, long, 'budgeting must not mutate the native snapshot');
});
