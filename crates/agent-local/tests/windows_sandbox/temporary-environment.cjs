const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const access = process.argv[2];
const identity = require('./expected-identity.json');
const denied = error => ['EACCES', 'EPERM'].includes(error.code);

async function verify() {
  for (const [key, value] of Object.entries(identity)) {
    assert.equal(process.env[key] ?? null, value, `${key} changed at AppContainer startup`);
  }
  const temporary = fs.realpathSync(os.tmpdir());
  for (const existing of [temporary, process.cwd()]) {
    assert.equal(fs.realpathSync.native(existing).toLowerCase(), fs.realpathSync(existing).toLowerCase());
    assert.equal((await fs.promises.realpath(existing)).toLowerCase(), fs.realpathSync(existing).toLowerCase());
  }
  for (const key of ['TEMP', 'TMP', 'TMPDIR']) {
    assert.equal(fs.realpathSync(process.env[key]).toLowerCase(), temporary.toLowerCase(), key);
  }
  if (access === 'workspace_write') {
    assert.equal(temporary.toLowerCase(), fs.realpathSync('.openharness-tmp/tmp').toLowerCase());
  } else {
    assert.ok(!temporary.toLowerCase().startsWith(process.cwd().toLowerCase() + path.sep));
    assert.throws(() => fs.writeFileSync('input.txt', 'forbidden'), denied);
  }
  const directory = fs.mkdtempSync(path.join(temporary, 'node-temp-probe-'));
  try {
    assert.equal(fs.realpathSync.native(directory).toLowerCase(), directory.toLowerCase());
    assert.equal((await fs.promises.realpath(directory)).toLowerCase(), directory.toLowerCase());
    const file = path.join(directory, '临时文件.txt');
    fs.writeFileSync(file, 'temporary content');
    assert.equal(fs.readFileSync(file, 'utf8'), 'temporary content');
  } finally {
    fs.rmSync(directory, { recursive: true });
  }
  assert.equal(fs.readFileSync('input.txt', 'utf8'), 'project input');
  assert.throws(() => fs.readFileSync('.env'), denied);
  assert.throws(() => fs.readFileSync('../outside/secret.txt'), denied);
  assert.throws(() => fs.writeFileSync('../outside/forbidden.txt', 'no'), denied);
  return { temporary };
}

async function main() {
  const parent = await verify();
  if (process.argv[3] === 'child') {
    console.log(JSON.stringify(parent));
  } else {
    const child = spawnSync(process.execPath, [__filename, access, 'child'], {
      encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 10_000,
    });
    assert.ifError(child.error);
    assert.equal(child.status, 0, child.stderr);
    console.log(JSON.stringify({ parent, child: JSON.parse(child.stdout) }));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
