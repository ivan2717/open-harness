import { accessSync, constants, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { COMPUTER_APP_NAME } from './app-transport.mjs';

const root = path.dirname(fileURLToPath(import.meta.url));
export function computerEndpoint(environment = process.env, platform = process.platform) {
  // macOS uses its independently authorized app; Windows uses a native stdio
  // helper. Host configuration is trusted, never supplied in model arguments.
  return environment.OPENHARNESS_COMPUTER_APP || environment.OPENHARNESS_COMPUTER_HELPER || (platform === 'win32' ? path.join(root, 'windows', 'bin', 'OpenHarnessComputerUse.exe') : path.join(root, 'bin', `${COMPUTER_APP_NAME}.app`));
}

export function requireComputerPlatform(platform = process.platform) {
  if (!['darwin', 'win32'].includes(platform)) throw new Error('Computer Use requires macOS 14+ or Windows 10/11 with an interactive desktop.');
}

export function requireComputerEndpoint(endpoint) {
  if (!path.isAbsolute(endpoint)) throw new Error('Computer Use component must have an absolute path.');
  const executable = endpoint.endsWith('.app') ? path.join(endpoint, 'Contents', 'MacOS', 'OpenHarnessComputerUse') : endpoint;
  try {
    accessSync(executable, constants.X_OK);
    if (!statSync(executable).isFile()) throw new Error('Not a file.');
    if (endpoint.endsWith('.app') && !statSync(path.join(endpoint, 'Contents', 'Info.plist')).isFile()) throw new Error('Missing app manifest.');
  } catch {
    throw new Error('Open Harness Computer Use is missing or not executable. Prepare the bundled computer component before using it.');
  }
}
