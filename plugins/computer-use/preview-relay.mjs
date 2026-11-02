import { createConnection } from 'node:net';

// No frame files, public listener or MCP messages. The desktop owns the token.
export class PreviewRelay {
  constructor(endpoint, control) {
    this.control = control; this.socket = null; this.enabled = false; this.latest = null; this.metadata = null; this.closed = false;
    const match = /^(\d{1,5}):([a-f0-9]{64})$/.exec(endpoint ?? '');
    if (!match || +match[1] < 1 || +match[1] > 65535) return;
    this.connect(+match[1], match[2]);
  }
  connect(port, token) {
    if (this.closed) return;
    const socket = createConnection({ host: '127.0.0.1', port }); this.socket = socket;
    let buffer = '';
    socket.setEncoding('utf8'); socket.setTimeout(3000, () => socket.destroy());
    socket.on('connect', () => { socket.setTimeout(0); socket.write(JSON.stringify({ token }) + '\n'); });
    socket.on('error', () => socket.destroy());
    socket.on('data', chunk => {
      buffer += chunk;
      if (buffer.length > 4096) return socket.destroy();
      let end;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
        try {
          const message = JSON.parse(line);
          if (typeof message.enabled !== 'boolean') throw new Error();
          this.enabled = message.enabled; this.control(this.enabled);
          if (this.metadata) this.send(this.metadata);
        } catch { socket.destroy(); }
      }
    });
    socket.on('drain', () => { const latest = this.latest; this.latest = null; if (latest) this.send(latest); });
    socket.on('close', () => {
      this.enabled = false; this.latest = null; this.control(false);
      if (!this.closed) { this.retry = setTimeout(() => this.connect(port, token), 2000); this.retry.unref(); }
    });
  }
  send(value) {
    if (!value || typeof value !== 'object' || typeof value.active !== 'boolean') return;
    const { jpeg, pointer, ...metadata } = value;
    this.metadata = metadata;
    const socket = this.socket;
    if (!socket || socket.connecting || socket.destroyed) return;
    const packet = this.enabled ? value : metadata;
    // Keep at most one unsent frame; never queue a video behind slow UI work.
    if (socket.writableLength > 900_000) { this.latest = packet; return; }
    socket.write(JSON.stringify(packet) + '\n');
  }
  stop() { this.closed = true; clearTimeout(this.retry); this.socket?.destroy(); this.latest = null; this.metadata = null; }
}

// Browser previews use the same local display channel. At most one capture is
// in flight; metadata checks keep close/revoke detection working while hidden.
// The callback must only inspect the already selected, authorized target and
// must not refresh a model snapshot/revision or activate a browser tab.
export class BrowserPreview {
  constructor(endpoint, capture, { interval = 500, relayFactory = (endpoint, control) => new PreviewRelay(endpoint, control) } = {}) {
    this.capture = capture; this.interval = interval; this.active = false; this.closed = false; this.epoch = 0; this.busy = false;
    this.relay = relayFactory(endpoint, () => this.schedule(0));
  }
  refresh() {
    if (this.closed) return;
    this.active = true;
    this.schedule(0);
  }
  schedule(delay) {
    clearTimeout(this.timer);
    if (this.closed || !this.active || this.busy) return;
    this.timer = setTimeout(() => void this.tick(), delay); this.timer.unref();
  }
  async tick() {
    if (this.closed || !this.active || this.busy) return;
    this.busy = true;
    const epoch = this.epoch;
    try {
      const state = await this.capture(this.relay.enabled);
      if (epoch !== this.epoch || this.closed) return;
      this.active = state.active === true;
      this.relay.send(state);
    } catch {
      if (epoch === this.epoch && !this.closed) this.relay.send({ active: true, title: '浏览器预览', unavailable: true });
    } finally {
      this.busy = false;
      this.schedule(this.relay.enabled ? this.interval : 1500);
    }
  }
  clear() { this.epoch++; this.active = false; clearTimeout(this.timer); this.relay.send({ active: false }); }
  stop() { this.clear(); this.closed = true; this.relay.stop(); }
}
