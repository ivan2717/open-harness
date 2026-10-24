import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import { findChromeExecutable } from './windows-chrome.mjs';

// Only an approved chrome_open may launch the user's browser. Status checks and
// other observation methods must never start apps as a side effect.
export async function launchChrome({ platform = process.platform, spawnProcess = spawn, findExecutable = findChromeExecutable } = {}) {
  if (platform === 'win32') {
    const executable = await findExecutable();
    await new Promise((resolve, reject) => {
      // Starting the browser process must not open a foreground user tab. The
      // extension creates the requested tab only after it has connected.
      const child = spawnProcess(executable, ['--no-startup-window'], { detached: true, stdio: 'ignore', windowsHide: true });
      child.once('error', () => reject(new Error('Google Chrome could not be started.')));
      child.once('spawn', () => { child.unref(); resolve(); });
    });
    return;
  }
  if (platform !== 'darwin') throw new Error('Automatic Chrome launch currently requires macOS or Windows.');
  await new Promise((resolve, reject) => {
    const child = spawnProcess('/usr/bin/open', ['-g', '-b', 'com.google.Chrome'], { stdio: 'ignore' });
    const timer = setTimeout(() => { child.kill('SIGTERM'); reject(new Error('Chrome launch timed out.')); }, 5000);
    child.once('error', () => { clearTimeout(timer); reject(new Error('Chrome could not be started. Install Google Chrome and prepare its connection first.')); });
    child.once('close', code => { clearTimeout(timer); if (code === 0) resolve(); else reject(new Error('Google Chrome could not be opened.')); });
  });
}
export async function requestWithLaunch(bridge, method, args, { launch = launchChrome, wait = delay, attempts = 20, shouldCancel = () => false } = {}) {
  const check = () => { if (shouldCancel()) throw new Error('Chrome operation cancelled.'); };
  check();
  try { await bridge.connect(); }
  catch (original) {
    if (method !== 'chrome_open') throw original;
    check(); await launch();
    let connected = false;
    for (let attempt = 0; attempt < attempts; attempt++) {
      await wait(500); check();
      try { await bridge.connect(); connected = true; break; } catch { /* Startup may still be connecting the extension. */ }
    }
    if (!connected) throw new Error('Chrome started, but its Open Harness extension did not connect. Complete extension setup or open its toolbar popup and reconnect.');
  }
  // Never retry a dispatched operation: only the connection phase is retried.
  check();
  return bridge.request(method, args);
}
