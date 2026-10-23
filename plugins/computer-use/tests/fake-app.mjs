import { readFileSync, statSync } from 'node:fs';
import { connect } from 'node:net';
import { createInterface } from 'node:readline';
import path from 'node:path';

const [directory, mode = 'normal'] = process.argv.slice(2);
if (mode === 'no-connect') await new Promise(() => { setInterval(() => {}, 1000); });
const token = readFileSync(path.join(directory, 'token'), 'utf8');
const modes = ['.', 'token', 'control.sock'].map(file => statSync(path.join(directory, file)).mode & 0o777);
const socket = connect(path.join(directory, 'control.sock'));
socket.on('error', () => process.exit(0));
socket.on('close', () => process.exit(0));
socket.once('connect', () => {
  socket.write(JSON.stringify({ protocol: 1, token: mode === 'bad-token' ? '0'.repeat(64) : token, pid: process.pid, bundle_id: mode === 'bad-identity' ? 'dev.openharness.desktop' : 'dev.openharness.computeruse' }) + '\n');
});
for await (const line of createInterface({ input: socket })) {
  const request = JSON.parse(line);
  socket.write(JSON.stringify({ id: request.id, result: { method: request.method, modes, selected: request.method === 'select_app' } }) + '\n');
}
