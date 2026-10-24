import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { EXTENSION_ID } from './constants.mjs';
import { findChromeExecutable } from './windows-chrome.mjs';

export function validateSetupUrl(value) {
  if (value === `chrome://extensions/?id=${EXTENSION_ID}`) return value;
  const url = new URL(value);
  if (url.origin !== 'https://chromewebstore.google.com' || url.username || url.password || url.search || url.hash || !new RegExp(`^/detail/(?:[a-z0-9][a-z0-9-]*/)?${EXTENSION_ID}/?$`).test(url.pathname)) throw new Error('Invalid Chrome extension setup URL.');
  return url.href;
}

export async function openSetupUrl(value, { findExecutable = findChromeExecutable, spawnProcess = spawn } = {}) {
  const url = validateSetupUrl(value);
  const executable = await findExecutable();
  await new Promise((resolve, reject) => {
    // This is an explicit user setup action. Use Chrome, not the default browser.
    const child = spawnProcess(executable, [url], { detached: true, stdio: 'ignore', windowsHide: true });
    child.once('error', () => reject(new Error('Google Chrome could not be opened.')));
    child.once('spawn', () => { child.unref(); resolve(); });
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    if (process.platform !== 'win32' || process.argv.length !== 3) throw new Error('Invalid Chrome setup invocation.');
    await openSetupUrl(process.argv[2]);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
