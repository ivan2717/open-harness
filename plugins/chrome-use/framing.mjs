import { MAX_REQUEST, MAX_RESPONSE } from './constants.mjs';
export function nativeFrame(value) {
  const payload = Buffer.from(JSON.stringify(value));
  if (payload.length > MAX_REQUEST) throw new Error('Native request is too large.');
  const header = Buffer.alloc(4); header.writeUInt32LE(payload.length);
  return Buffer.concat([header, payload]);
}
export class NativeDecoder {
  constructor(onMessage, max = MAX_RESPONSE) { this.buffer = Buffer.alloc(0); this.onMessage = onMessage; this.max = max; }
  push(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 4) {
      const size = this.buffer.readUInt32LE(0);
      if (!size || size > this.max) throw new Error('Invalid native message size.');
      if (this.buffer.length < size + 4) break;
      const payload = this.buffer.subarray(4, size + 4); this.buffer = this.buffer.subarray(size + 4);
      this.onMessage(JSON.parse(payload.toString('utf8')));
    }
    if (this.buffer.length > this.max + 4) throw new Error('Native frame is too large.');
  }
}
export class LineDecoder {
  constructor(onMessage, max = MAX_REQUEST) { this.buffer = ''; this.onMessage = onMessage; this.max = max; }
  push(chunk) {
    this.buffer += chunk;
    let end;
    while ((end = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, end); this.buffer = this.buffer.slice(end + 1);
      if (Buffer.byteLength(line) > this.max) throw new Error('Message is too large.');
      if (line.trim()) this.onMessage(JSON.parse(line));
    }
    if (Buffer.byteLength(this.buffer) > this.max) throw new Error('Message is too large.');
  }
}
