const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const { once } = require('node:events');

console.error(`Node ${process.version}, libuv ${process.versions.uv}: pipe regression started`);

// Every child must inherit the same file boundary, not merely start correctly.
const boundary = `
  const fs = require('node:fs');
  const path = require('node:path');
  const assert = require('node:assert/strict');
  const denied = error => ['EACCES', 'EPERM'].includes(error.code);
  assert.equal(fs.readFileSync('input.txt', 'utf8'), 'project input');
  assert.throws(() => fs.readFileSync('../outside/secret.txt'), denied);
  assert.throws(() => fs.writeFileSync('../outside/child-write.txt', 'no'), denied);
  assert.throws(() => fs.writeFileSync('input.txt', 'no'), denied);
  assert.throws(() => fs.readFileSync('.env'), denied);
`;
const exchange = boundary + `
  const input = fs.readFileSync(0, 'utf8');
  process.stdout.write('stdout:' + input);
  process.stderr.write('stderr:' + input);
`;
const active = new Set();

function start(script) {
  const child = spawn(process.execPath, ['-e', script], { stdio: 'pipe' });
  active.add(child);
  child.once('close', () => active.delete(child));
  return child;
}

async function main() {
  console.error('spawnSync pipes');
  const sync = spawnSync(process.execPath, ['-e', exchange], {
    stdio: 'pipe', encoding: 'utf8', input: 'sync', timeout: 10_000,
  });
  assert.ifError(sync.error);
  assert.equal(sync.status, 0, sync.stderr);
  assert.equal(sync.stdout, 'stdout:sync');
  assert.equal(sync.stderr, 'stderr:sync');

  console.error('spawnSync ignored stdin');
  const ignored = spawnSync(process.execPath, ['-e', boundary + `
    process.stdout.write('ignored stdin');
    process.stderr.write('ignored stdin stderr');
  `], {
    stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', timeout: 10_000,
  });
  assert.ifError(ignored.error);
  assert.equal(ignored.status, 0, ignored.stderr);
  assert.equal(ignored.stdout, 'ignored stdin');
  assert.equal(ignored.stderr, 'ignored stdin stderr');

  console.error('spawnSync ignored output');
  const ignoredOutput = spawnSync(process.execPath, ['-e', boundary + `
    process.stdout.write('discarded stdout');
    process.stderr.write('discarded stderr');
  `], { stdio: 'ignore', timeout: 10_000 });
  assert.ifError(ignoredOutput.error);
  assert.equal(ignoredOutput.status, 0);

  console.error('spawn pipes');
  const asyncChild = start(exchange);
  const closed = once(asyncChild, 'close');
  let stdout = '';
  let stderr = '';
  asyncChild.stdout.setEncoding('utf8').on('data', data => { stdout += data; });
  asyncChild.stderr.setEncoding('utf8').on('data', data => { stderr += data; });
  asyncChild.stdin.end('async');
  const [code, signal] = await closed;
  assert.equal(code, 0, stderr);
  assert.equal(signal, null);
  assert.equal(stdout, 'stdout:async');
  assert.equal(stderr, 'stderr:async');

  console.error('spawnSync timeout');
  const timedOut = spawnSync(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    stdio: 'pipe', encoding: 'utf8', timeout: 2_000,
  });
  assert.equal(timedOut.error?.code, 'ETIMEDOUT');
  assert.notEqual(timedOut.status, 0);
  assert.ok(timedOut.pid > 0, 'timeout must terminate a created child');

  console.error('spawn cancellation');
  const cancelled = start(boundary + `
    process.stdout.write('READY\\n');
    setInterval(() => {}, 1000);
  `);
  const cancelledClose = once(cancelled, 'close');
  let readyText = '';
  const ready = await Promise.race([
    new Promise(resolve => {
      cancelled.stdout.setEncoding('utf8').on('data', data => {
        readyText += data;
        if (readyText.includes('\n')) resolve(readyText);
      });
    }),
    cancelledClose.then(result => {
      throw new Error('Child exited before cancellation: ' + JSON.stringify(result));
    }),
  ]);
  assert.equal(ready, 'READY\n');
  assert.equal(cancelled.kill(), true);
  const [cancelCode, cancelSignal] = await cancelledClose;
  assert.equal(cancelled.killed, true);
  assert.notEqual(cancelCode, 0);
  assert.ok(cancelCode !== null || cancelSignal !== null);
  assert.equal(active.size, 0, 'all asynchronous children must be reaped');
  console.log('NODE_CHILD_PROCESS_PIPES_OK');
}

main().catch(error => {
  for (const child of active) child.kill();
  console.error(error);
  process.exitCode = 1;
});
