import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { AppTransport } from '../app-transport.mjs';
import { computerEndpoint } from '../endpoint.mjs';

const fake = fileURLToPath(new URL('./fake-app.mjs', import.meta.url));
function fixture(t, mode = 'normal', startupTimeoutMs = 2000) {
  let launched;
  let child;
  const transport = new AppTransport('/fixture/Open Harness Computer Use.app', {
    startupTimeoutMs,
    launch(command, args, options) {
      launched = { command, args, options };
      child = spawn(process.execPath, [fake, args.at(-1), mode], { stdio: 'ignore' });
      return child;
    },
  });
  const errors = [];
  transport.on('error', error => errors.push(error));
  t.after(() => { transport.kill(); child?.kill(); });
  return { transport, errors, launched: () => launched, child: () => child };
}

test('independent app launch authenticates a private channel and preserves JSON protocol', { timeout: 5000 }, async t => {
  const { transport, launched, errors } = fixture(t);
  const lines = createInterface({ input: transport.stdout });
  const responseLine = Promise.race([once(lines, 'line'), once(transport, 'error').then(([error]) => { throw error; })]);
  transport.stdin.write(JSON.stringify({ id: 7, method: 'status', params: {} }) + '\n');
  const [line] = await responseLine;
  const response = JSON.parse(line);
  assert.equal(response.id, 7);
  assert.equal(response.result.method, 'status');
  assert.deepEqual(response.result.modes, [0o700, 0o600, 0o600]);
  assert.equal(launched().command, '/usr/bin/open');
  assert.deepEqual(launched().args.slice(0, -1), ['-g', '-j', '-n', '-W', '/fixture/Open Harness Computer Use.app', '--args', '--socket-dir']);
  assert.equal(errors.length, 0);
  const directory = transport.directory;
  transport.kill();
  await transport.cleanup();
  await assert.rejects(access(directory));
});

for (const mode of ['bad-token', 'bad-identity']) {
  test(`rejects ${mode} before any tool request reaches the peer`, { timeout: 5000 }, async t => {
    const { transport, errors, launched } = fixture(t, mode, 1000);
    let output = '';
    transport.stdout.on('data', chunk => { output += chunk; });
    transport.stdin.write('{"id":1,"method":"type","params":{"text":"not delivered"}}\n');
    await new Promise(resolve => transport.once('exit', resolve));
    assert.equal(transport.socket, null);
    assert.equal(output, '');
    assert.equal(launched()?.command, '/usr/bin/open', 'the peer must have connected before rejection is counted');
    assert.ok(errors.length);
  });
}

test('startup timeout closes a nonconnecting app launcher and removes the private channel', { timeout: 5000 }, async t => {
  const { transport, errors, child } = fixture(t, 'no-connect', 250);
  await new Promise(resolve => transport.once('exit', resolve));
  await transport.cleanup();
  assert.match(errors[0].message, /did not connect/);
  assert.equal(child().killed, true);
  await assert.rejects(access(transport.directory));
});

test('immediate cancellation cannot launch an app after asynchronous setup completes', { timeout: 5000 }, async () => {
  let launches = 0;
  const transport = new AppTransport('/fixture/Open Harness Computer Use.app', { launch() { launches++; throw new Error('must not launch'); } });
  transport.on('error', () => {});
  transport.kill();
  // start() performs filesystem work before checking cancellation.
  for (let i = 0; i < 40; i++) {
    if (transport.directory) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  await new Promise(resolve => setTimeout(resolve, 30));
  await transport.cleanup();
  assert.equal(launches, 0);
  await assert.rejects(access(transport.directory));
});

test('production app endpoint wins over a legacy helper and raw execution is explicit', () => {
  assert.match(computerEndpoint({}), /Open Harness Computer Use\.app$/);
  assert.equal(computerEndpoint({ OPENHARNESS_COMPUTER_APP: '/app/Computer.app', OPENHARNESS_COMPUTER_HELPER: '/test/helper' }), '/app/Computer.app');
  assert.equal(computerEndpoint({ OPENHARNESS_COMPUTER_HELPER: '/test/helper' }), '/test/helper');
});
