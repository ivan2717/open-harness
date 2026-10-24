import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { fileURLToPath } from 'node:url';
import { launchChrome, requestWithLaunch } from '../launch-chrome.mjs';
import { findChromeExecutable } from '../windows-chrome.mjs';
import { openSetupUrl, validateSetupUrl } from '../open-url.mjs';
import { EXTENSION_ID } from '../constants.mjs';
test('approved open starts Chrome in background with fixed arguments', async () => {
  let actual;
  await launchChrome({ platform: 'darwin', spawnProcess(command, args, options) { actual = { command, args, options }; const child = new EventEmitter(); queueMicrotask(() => child.emit('close', 0)); return child; } });
  assert.deepEqual(actual, { command: '/usr/bin/open', args: ['-g', '-b', 'com.google.Chrome'], options: { stdio: 'ignore' } });
});
test('Windows approved open delegates browser lifetime to the fixed native launcher', async () => {
  let actual;
  await launchChrome({ platform: 'win32', findExecutable: async () => 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe', spawnProcess(command, args, options) { actual = { command, args, options }; const child = new EventEmitter(); queueMicrotask(() => child.emit('close', 0)); return child; } });
  assert.equal(actual.command, fileURLToPath(new URL('../launch.exe', import.meta.url)));
  assert.deepEqual(actual.args, ['--launch-chrome', 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe']);
  assert.deepEqual(actual.options, { stdio: 'ignore', windowsHide: true });
});
test('Windows Chrome launch reports a refused breakaway instead of pretending startup succeeded', async () => {
  for (const outcome of ['error', 'close']) {
    let count = 0;
    await assert.rejects(launchChrome({ platform: 'win32', findExecutable: async () => 'C:\\Chrome\\chrome.exe', spawnProcess() { count++; const child = new EventEmitter(); queueMicrotask(() => outcome === 'error' ? child.emit('error', new Error('spawn failed')) : child.emit('close', 1)); return child; } }), /could not be started/);
    assert.equal(count, 1);
  }
});
test('Windows resolves custom Chrome App Paths as an executable, not a shell command', async () => {
  const calls = [];
  const executable = 'D:\\浏览器 & 100%tools\\Chrome\\chrome.exe';
  const found = await findChromeExecutable({ env: { SystemRoot: 'C:\\Windows' }, executeFile: async (command, args, options) => { calls.push({ command, args, options }); return { stdout: Buffer.from(executable, 'utf8').toString('base64') }; }, inspectFile: async value => { assert.equal(value, executable); return { isFile: () => true, isSymbolicLink: () => false }; } });
  assert.equal(found, executable);
  assert.equal(calls[0].command, 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe');
  assert.deepEqual(calls[0].args.slice(0, 4), ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command']);
  assert.equal(calls.every(call => call.args[4] === calls[0].args[4]), true);
  assert.equal(calls[0].args[4].includes(executable), false);
  assert.match(calls[0].args[4], /ToBase64String/);
  assert.match(calls[0].args[4], /GetValue\(''\)/);
  assert.deepEqual(calls.map(call => [call.options.env.OPENHARNESS_CHROME_HIVE, call.options.env.OPENHARNESS_CHROME_VIEW]), [['HKCU', '64'], ['HKCU', '32'], ['HKLM', '64'], ['HKLM', '32']]);
  assert.equal(calls[0].options.windowsHide, true);
});
test('Windows Chrome discovery ignores commands, remote shares and symbolic links', async () => {
  for (const value of ['"C:\\Chrome\\chrome.exe" --evil', '\\\\remote\\share\\chrome.exe', 'C:\\Chrome\\other.exe', 'chrome.exe']) {
    let read = false;
    await assert.rejects(findChromeExecutable({ env: { SystemRoot: 'C:\\Windows' }, executeFile: async () => ({ stdout: Buffer.from(value, 'utf8').toString('base64') }), inspectFile: async () => { read = true; return { isFile: () => true, isSymbolicLink: () => false }; } }), /not found/);
    assert.equal(read, false);
  }
  await assert.rejects(findChromeExecutable({ env: { LOCALAPPDATA: 'C:\\Users\\Tester\\AppData\\Local' }, inspectFile: async () => ({ isFile: () => true, isSymbolicLink: () => true }) }), /not found/);
});
test('Windows Chrome discovery falls back to the current user installation', async () => {
  const expected = 'C:\\Users\\中文 100% User\\AppData\\Local\\Google\\Chrome\\Application\\chrome.exe';
  assert.equal(await findChromeExecutable({ env: { SystemRoot: 'C:\\Windows', LOCALAPPDATA: 'C:\\Users\\中文 100% User\\AppData\\Local' }, executeFile: async () => { throw new Error('key absent'); }, inspectFile: async value => { assert.equal(value, expected); return { isFile: () => true, isSymbolicLink: () => false }; } }), expected);
});
test('Windows Chrome discovery never interprets localized or malformed registry output as paths', async () => {
  for (const stdout of ['中文输出', 'C:\\Chrome\\chrome.exe', 'not-base64***']) {
    let read = false;
    await assert.rejects(findChromeExecutable({ env: { SystemRoot: 'C:\\Windows' }, executeFile: async () => ({ stdout }), inspectFile: async () => { read = true; } }), /not found/);
    assert.equal(read, false);
  }
});
test('Windows setup opens only the extension management or real store URL in Chrome', async () => {
  const url = `https://chromewebstore.google.com/detail/open-harness/${EXTENSION_ID}`;
  assert.equal(validateSetupUrl(`chrome://extensions/?id=${EXTENSION_ID}`), `chrome://extensions/?id=${EXTENSION_ID}`);
  for (const invalid of ['https://example.com', 'chrome://settings', `${url}?redirect=elsewhere`, url.replace(EXTENSION_ID, 'a'.repeat(32)), `https://name@chromewebstore.google.com/detail/${EXTENSION_ID}`]) assert.throws(() => validateSetupUrl(invalid), /Invalid Chrome extension setup URL/);
  let actual;
  await openSetupUrl(url, { findExecutable: async () => 'C:\\Chrome\\chrome.exe', spawnProcess(command, args, options) { actual = { command, args, options }; const child = new EventEmitter(); child.unref = () => {}; queueMicrotask(() => child.emit('spawn')); return child; } });
  assert.deepEqual(actual, { command: 'C:\\Chrome\\chrome.exe', args: [url], options: { detached: true, stdio: 'ignore', windowsHide: true } });
});
test('only connection setup retries; an action failure is never retried', async () => {
  let attempts = 0; let launched = 0; let requests = 0;
  const bridge = { async connect() { if (++attempts < 3) throw new Error('offline'); }, async request() { requests++; throw new Error('uncertain effect'); } };
  await assert.rejects(requestWithLaunch(bridge, 'chrome_open', { url: 'https://example.com' }, { launch: async () => { launched++; }, wait: async () => {}, attempts: 5 }), /uncertain effect/);
  assert.equal(launched, 1); assert.equal(requests, 1); assert.equal(attempts, 3);
});
test('status and reads cannot launch apps', async () => {
  let launched = false;
  const bridge = { async connect() { throw new Error('offline'); } };
  for (const method of ['chrome_status', 'chrome_snapshot', 'chrome_action']) await assert.rejects(requestWithLaunch(bridge, method, {}, { launch: async () => { launched = true; } }), /offline/);
  assert.equal(launched, false);
});
test('cancellation during Chrome startup does not dispatch the requested operation', async () => {
  let cancelled = false; let requests = 0;
  const bridge = { async connect() { throw new Error('offline'); }, async request() { requests++; } };
  await assert.rejects(requestWithLaunch(bridge, 'chrome_open', {}, { launch: async () => {}, wait: async () => { cancelled = true; }, shouldCancel: () => cancelled }), /cancelled/);
  assert.equal(requests, 0);
});
