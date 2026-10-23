// Rasterizes the small geometric icon in assets/icon.svg without a platform
// graphics dependency. Supersampling preserves the same geometry at all sizes.
import { deflateSync } from 'node:zlib';
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const table = Array.from({ length: 256 }, (_, value) => { for (let bit = 0; bit < 8; bit++) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1; return value >>> 0; });
const crc32 = buffer => { let crc = 0xffffffff; for (const byte of buffer) crc = table[(crc ^ byte) & 255] ^ (crc >>> 8); return (crc ^ 0xffffffff) >>> 0; };
const chunk = (type, data) => { const name = Buffer.from(type); const size = Buffer.alloc(4); size.writeUInt32BE(data.length); const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([name, data]))); return Buffer.concat([size, name, data, crc]); };
const points = [[34, 64], [46, 64], [54, 43], [65, 84], [74, 60], [94, 60]];
function segmentDistance(x, y, a, b) { const dx = b[0] - a[0]; const dy = b[1] - a[1]; const t = Math.max(0, Math.min(1, ((x - a[0]) * dx + (y - a[1]) * dy) / (dx * dx + dy * dy))); return Math.hypot(x - a[0] - t * dx, y - a[1] - t * dy); }
function pixel(x, y) {
  const inside = x >= 16 && x <= 112 && y >= 16 && y <= 112 && Math.hypot(Math.max(Math.abs(x - 64) - 24, 0), Math.max(Math.abs(y - 64) - 24, 0)) <= 24;
  if (!inside) return [0, 0, 0, 0];
  if (points.slice(1).some((point, index) => segmentDistance(x, y, points[index], point) <= 3)) return [255, 255, 255, 255];
  return [61, 89, 71, 255];
}
await mkdir(path.join(root, 'extension/icons'), { recursive: true });
for (const size of [16, 32, 48, 128]) {
  const data = Buffer.alloc(size * (size * 4 + 1)); const samples = 4;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const total = [0, 0, 0, 0];
    for (let sy = 0; sy < samples; sy++) for (let sx = 0; sx < samples; sx++) {
      const rgba = pixel((x + (sx + .5) / samples) * 128 / size, (y + (sy + .5) / samples) * 128 / size);
      for (let channel = 0; channel < 3; channel++) total[channel] += rgba[channel] * rgba[3] / 255;
      total[3] += rgba[3];
    }
    const alpha = total[3] / (samples * samples); const index = y * (size * 4 + 1) + 1 + x * 4;
    for (let channel = 0; channel < 3; channel++) data[index + channel] = alpha ? Math.round(total[channel] / (samples * samples) * 255 / alpha) : 0;
    data[index + 3] = Math.round(alpha);
  }
  const header = Buffer.alloc(13); header.writeUInt32BE(size, 0); header.writeUInt32BE(size, 4); header[8] = 8; header[9] = 6;
  const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', header), chunk('IDAT', deflateSync(data)), chunk('IEND', Buffer.alloc(0))]);
  await writeFile(path.join(root, `extension/icons/icon-${size}.png`), png);
}
