import { accessSync, constants, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { COMPUTER_APP_NAME } from './app-transport.mjs';

const root = path.dirname(fileURLToPath(import.meta.url));
export function computerEndpoint(environment = process.env) {
  // Direct helper execution exists solely for isolated fixture/protocol tests.
  // Production desktop hosts always pass the independently launched app path.
  return environment.OPENHARNESS_COMPUTER_APP || environment.OPENHARNESS_COMPUTER_HELPER || path.join(root, 'bin', `${COMPUTER_APP_NAME}.app`);
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
