import { createInterface } from 'node:readline';
import { appendFileSync } from 'node:fs';
let selected = false;
const log = process.argv[2];
for await (const line of createInterface({ input: process.stdin })) {
  const { id, method, params } = JSON.parse(line);
  appendFileSync(log, `${method}\n`);
  if (method === 'mark_eof') process.stdin.once('end', () => appendFileSync(log, 'graceful_eof\n'));
  if (method === 'hang') await new Promise(() => {});
  if (method === 'slow') await new Promise(resolve => setTimeout(resolve, params.delay));
  if (method === 'select_app') selected = true;
  if (method === 'bad_json') { process.stdout.write('not-json\n'); continue; }
  if (method === 'wrong_id') { process.stdout.write(JSON.stringify({ id: -1, result: {} }) + '\n'); continue; }
  if (method === 'fail') { process.stdout.write(JSON.stringify({ id, error: { code: 'STATE_CHANGED', message: 'Observe again.' } }) + '\n'); continue; }
  if (method === 'crash') process.exit(1);
  if (method === 'unicode_chunks') {
    const data = Buffer.from(JSON.stringify({ id, result: { text: '中文🙂' } }) + '\n');
    for (const byte of data) {
      process.stdout.write(Buffer.from([byte]));
      await new Promise(resolve => setTimeout(resolve, 1));
    }
    continue;
  }
  process.stdout.write(JSON.stringify({ id, result: { method, selected, pid: process.pid } }) + '\n');
}
