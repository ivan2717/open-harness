import test from 'node:test';
import assert from 'node:assert/strict';
import { tools, validate, parseKey } from '../tools.mjs';

const snapshot_id = 'fresh-snapshot';

test('window selection and all mutating tools have bounded, explicit schemas', () => {
  validate('computer_windows', {});
  validate('computer_select_app', { bundle_id: 'app.fixture', pid: 12, window_id: 456 });
  for (const window_id of [0, -1, 1.5, '456', Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => validate('computer_select_app', { bundle_id: 'app.fixture', pid: 12, window_id }));
  }
  for (const tool of tools.filter(tool => /computer_(click|type|key|scroll|drag|set_value|action)$/.test(tool.name))) {
    assert.ok(tool.inputSchema.required.includes('snapshot_id'), tool.name);
    assert.equal(tool.inputSchema.additionalProperties, false);
  }
});

test('pointer options accept bounded gestures and reject malformed modifiers', () => {
  validate('computer_click', { snapshot_id, x: 20, y: 30, button: 'right', click_count: 3, modifiers: ['control', 'shift'] });
  validate('computer_click', { snapshot_id, ref: 'e1', button: 'middle', click_count: 2, modifiers: [] });
  validate('computer_drag', { snapshot_id, from_x: 0, from_y: 1, to_x: 200, to_y: 100, modifiers: ['alt'] });
  for (const options of [{ button: 'secondary' }, { click_count: 0 }, { click_count: 4 }, { click_count: 1.5 },
    { modifiers: 'shift' }, { modifiers: ['shift', 'shift'] }, { modifiers: ['cmd'] }, { modifiers: [null] }, { modifiers: null }]) {
    assert.throws(() => validate('computer_click', { snapshot_id, ref: 'e1', ...options }));
  }
  assert.throws(() => validate('computer_drag', { snapshot_id, from_x: 0, from_y: 0, to_x: Infinity, to_y: 1 }));
  assert.throws(() => validate('computer_drag', { snapshot_id, from_x: 0, from_y: 0, to_x: 1 }));
});

test('key chords normalize platform aliases and reject ambiguous or system chords', () => {
  for (const key of ['enter', 'shift+tab', 'meta+a', 'ctrl+shift+z', 'option+left', 'f12', 'space', 'control+9']) validate('computer_key', { snapshot_id, key });
  assert.deepEqual(parseKey('Command+Shift+A'), { key: 'a', modifiers: ['meta', 'shift'] });
  for (const key of ['cmd+tab', 'alt+tab', 'meta+space', 'control+alt+delete', 'alt+f4', 'ctrl+escape',
    'control+control+a', 'ctrl+control+a', 'shift+', '+a', 'a+b', 'f13', 'a b', 'shift+unknown']) {
    assert.throws(() => validate('computer_key', { snapshot_id, key }), key);
  }
});

test('accessibility actions require exact refs and bounded values, including clearing', () => {
  validate('computer_set_value', { snapshot_id, ref: 'e1', value: '' });
  validate('computer_set_value', { snapshot_id, ref: 'e1', value: '中文🙂' });
  validate('computer_action', { snapshot_id, ref: 'e1', action: 'AXShowMenu' });
  validate('computer_action', { snapshot_id, ref: 'e1', action: 'Expand' });
  for (const value of [null, 3, 'x'.repeat(2001), 'a\0b']) assert.throws(() => validate('computer_set_value', { snapshot_id, ref: 'e1', value }));
  assert.throws(() => validate('computer_action', { snapshot_id, ref: 'e1', action: '' }));
  assert.throws(() => validate('computer_action', { snapshot_id, action: 'Invoke' }));
  assert.throws(() => validate('computer_action', { snapshot_id, ref: 'e1', action: 'Invoke', retry: true }));
});
