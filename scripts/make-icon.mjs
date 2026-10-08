// Draws the Colony Command icon (a bot on a dark tile) and writes
// app/src-tauri/icons/icon.png and icon.ico. No dependencies, so it runs
// without native modules:  node scripts/make-icon.mjs
import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const N = 256;
const SS = 4; // supersamples per axis, for smooth edges

const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
const TILE = hex("#18211c");
const RING = hex("#5cc27a");
const BODY = hex("#9b7ce0");
const INK = hex("#0f1512");
const WHITE = [255, 255, 255];

const inRoundRect = (x, y, x0, y0, x1, y1, r) => {
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r && x >= x0 && x <= x1 && y >= y0 && y <= y1;
};
const inEllipse = (x, y, cx, cy, rx, ry) => ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1;

// Painter's order: later layers cover earlier ones. Returns [rgb, alpha] or null.
function shade(x, y) {
  let c = null;
  if (inRoundRect(x, y, 8, 8, 248, 248, 56)) c = [TILE, 1];
  if (!c) return null;
  if (inEllipse(x, y, 128, 206, 86, 24)) c = [RING, 0.75];
  if (inEllipse(x, y, 128, 128, 74, 70)) c = [INK, 1];
  if (inEllipse(x, y, 128, 128, 66, 62)) c = [BODY, 1];
  for (const ex of [100, 156]) {
    if (inEllipse(x, y, ex, 112, 17, 17)) c = [WHITE, 1];
    if (inEllipse(x, y, ex + 3, 117, 8, 8)) c = [INK, 1];
  }
  return c;
}

const px = Buffer.alloc(N * N * 4);
for (let y = 0; y < N; y++) {
  for (let x = 0; x < N; x++) {
    let r = 0, g = 0, b = 0, a = 0;
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const s = shade(x + (sx + 0.5) / SS, y + (sy + 0.5) / SS);
        if (!s) continue;
        const [rgb, alpha] = s;
        // Blend translucent layers over the tile.
        const base = alpha < 1 ? TILE : rgb;
        r += rgb[0] * alpha + base[0] * (1 - alpha);
        g += rgb[1] * alpha + base[1] * (1 - alpha);
        b += rgb[2] * alpha + base[2] * (1 - alpha);
        a += 1;
      }
    }
    const i = (y * N + x) * 4;
    if (a) {
      px[i] = Math.round(r / a);
      px[i + 1] = Math.round(g / a);
      px[i + 2] = Math.round(b / a);
    }
    px[i + 3] = Math.round((a / (SS * SS)) * 255);
  }
}

// --- PNG ---
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const byte of buf) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
};
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(N, 0);
ihdr.writeUInt32BE(N, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
const raw = Buffer.alloc(N * (N * 4 + 1));
for (let y = 0; y < N; y++) px.copy(raw, y * (N * 4 + 1) + 1, y * N * 4, (y + 1) * N * 4);
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

// --- ICO: one 256x256 PNG entry (supported since Windows Vista) ---
const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0);
header.writeUInt16LE(1, 2); // icon
header.writeUInt16LE(1, 4); // one image
const entry = Buffer.alloc(16);
entry[0] = 0; // 256 px
entry[1] = 0;
entry.writeUInt16LE(1, 4); // planes
entry.writeUInt16LE(32, 6); // bpp
entry.writeUInt32LE(png.length, 8);
entry.writeUInt32LE(6 + 16, 12);
const ico = Buffer.concat([header, entry, png]);

const out = new URL("../app/src-tauri/icons/", import.meta.url);
writeFileSync(new URL("icon.png", out), png);
writeFileSync(new URL("icon.ico", out), ico);
console.log(`icon.png ${png.length} bytes, icon.ico ${ico.length} bytes`);
