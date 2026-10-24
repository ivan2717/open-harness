import path from 'node:path';

// Keep the wire configuration compatible while using the OS's local IPC
// transport. Do not accept arbitrary devices, remote pipes or TCP addresses.
export function validateSocketPath(value, platform = process.platform) {
  if (typeof value !== 'string' || value.includes('\0')) throw new Error('Invalid Chrome bridge endpoint.');
  if (platform === 'win32') {
    if (!/^\\\\\.\\pipe\\openharness-chrome-[a-f0-9]{16,64}$/.test(value)) throw new Error('Invalid Chrome bridge named pipe.');
    return;
  }
  if (!path.isAbsolute(value) || Buffer.byteLength(value) > 100) throw new Error('Invalid Chrome bridge socket path.');
}
