import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { PreviewRelay } from '../preview-relay.mjs';
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate) { for (let i=0;i<100;i++) { if (predicate()) return; await wait(20); } throw new Error('Preview timed out'); }
test('local relay authenticates, pauses frames and shuts down without replaying frame data', async t => {
  let peer; const packets = [], controls = [];
  const server = createServer(socket => {
    peer=socket; let buffer=''; socket.setEncoding('utf8');
    socket.on('data', chunk => { buffer+=chunk; let end; while ((end=buffer.indexOf('\n'))>=0) { packets.push(JSON.parse(buffer.slice(0,end))); buffer=buffer.slice(end+1); } });
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const token='a'.repeat(64);
  const relay=new PreviewRelay(`${server.address().port}:${token}`, enabled => controls.push(enabled));
  t.after(() => { relay.stop(); peer?.destroy(); server.close(); });
  await until(() => packets.length===1); assert.deepEqual(packets[0],{token});
  const frame={active:true,title:'Test',width:10,height:10,jpeg:'/9j/abcd'};
  relay.send(frame); await until(() => packets.length===2); assert.equal(packets[1].jpeg,undefined);
  peer.write('{"enabled":true}\n'); await until(() => controls.at(-1)===true);
  relay.send(frame); await until(() => packets.at(-1)?.jpeg===frame.jpeg);
  peer.write('{"enabled":false}\n'); await until(() => controls.at(-1)===false);
  relay.send(frame); await wait(30); assert.equal(packets.at(-1).jpeg,undefined);
  assert.equal(relay.metadata.jpeg,undefined);
  peer.destroy(); await until(() => relay.socket.destroyed); assert.equal(controls.at(-1),false);
});

test('browser preview pauses capture, bounds in-flight work and drops late frames after release', async () => {
  const { BrowserPreview } = await import('../preview-relay.mjs');
  const packets = []; const requests = []; let resolveCapture; let control;
  const relay = { enabled: true, send: value => packets.push(value), stop() {} };
  const preview = new BrowserPreview('unused', frame => { requests.push(frame); return new Promise(resolve => { resolveCapture = resolve; }); }, {
    interval: 10,
    relayFactory: (_, callback) => { control = callback; return relay; },
  });
  const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
  try {
    preview.refresh(); await pause(15);
    preview.refresh(); await pause(20);
    assert.equal(requests.length, 1);
    preview.clear(); resolveCapture({ active: true, jpeg: '/9j/late' }); await pause(20);
    assert.deepEqual(packets, [{ active: false }]);
    relay.enabled = false; control(false); preview.refresh(); await pause(15);
    assert.equal(requests.at(-1), false);
    resolveCapture({ active: true, title: 'Chrome' }); await pause(10);
    relay.enabled = true; control(true); await pause(15);
    assert.equal(requests.at(-1), true);
    resolveCapture({ active: false }); await pause(20);
    const count = requests.length; await pause(25);
    assert.equal(requests.length, count);
  } finally { preview.stop(); }
});
