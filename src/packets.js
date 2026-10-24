// ErsatzTV graphics packets (script element, `format: packet`):
// 4 bytes magic "ETVG" (0x47565445 big endian), 2 bytes version 1,
// 1 byte type, 4 bytes payload length, payload. All big endian.
//
// Every packet yields one frame; `Repeat(n)` yields the previous frame
// n + 1 times. `Rectangles` erases the canvas first, so it must carry every
// visible pixel, not only the changed ones.

export const MAGIC = 0x47565445;
export const VERSION = 1;
export const Type = { Full: 0, Repeat: 1, Clear: 2, Rectangles: 3 };

function header(type, length) {
  const buffer = Buffer.alloc(11);
  buffer.writeUInt32BE(MAGIC, 0);
  buffer.writeUInt16BE(VERSION, 4);
  buffer.writeUInt8(type, 6);
  buffer.writeUInt32BE(length, 7);
  return buffer;
}

export function clearPacket() {
  return header(Type.Clear, 0);
}

/** `frames` frames of the previous picture (at least 1). */
export function repeatPacket(frames) {
  const payload = Buffer.alloc(4);
  payload.writeUInt32BE(Math.max(0, frames - 1), 0);
  return Buffer.concat([header(Type.Repeat, 4), payload]);
}

/** Rectangles `{ x, y, w, h, pixels }` of RGBA/BGRA; the rest of the canvas is transparent. */
export function rectanglesPacket(rects) {
  const parts = [Buffer.alloc(2)];
  parts[0].writeUInt16BE(rects.length, 0);

  for (const { x, y, w, h, pixels } of rects) {
    if (pixels.length !== w * h * 4) throw new Error("rectangle size mismatch");
    const meta = Buffer.alloc(8);
    meta.writeUInt16BE(x, 0);
    meta.writeUInt16BE(y, 2);
    meta.writeUInt16BE(w, 4);
    meta.writeUInt16BE(h, 6);
    parts.push(meta, pixels);
  }

  const payload = Buffer.concat(parts);
  return Buffer.concat([header(Type.Rectangles, payload.length), payload]);
}

/** Packet for a captured frame: its visible rectangles, or Clear. */
export function framePacket(frame) {
  return frame.rects.length ? rectanglesPacket(frame.rects) : clearPacket();
}

// Transparent gaps narrower than this stay inside one rectangle: fewer
// rectangles, a few more transparent pixels.
const GAP = 32;
const MAX_RECTS = 32;

/**
 * Boxes around the pixels with alpha > 0: bands of rows, split into column
 * runs, each trimmed to its own rows. A logo in one corner and a ticker at the
 * bottom become two small rectangles instead of one box over the whole frame.
 */
export function visibleRects(rgba, width, height) {
  const rowVisible = new Uint8Array(height);

  for (let y = 0; y < height; y++) {
    const end = (y + 1) * width * 4;
    for (let i = y * width * 4 + 3; i < end; i += 4) {
      if (rgba[i] !== 0) {
        rowVisible[y] = 1;
        break;
      }
    }
  }

  const rects = [];
  const colTop = new Int32Array(width);
  const colBottom = new Int32Array(width);

  for (let y = 0; y < height;) {
    if (!rowVisible[y]) {
      y++;
      continue;
    }

    // One band: visible rows with gaps shorter than GAP.
    const top = y;
    let bottom = y;
    for (let gap = 0; y < height && gap < GAP; y++) {
      if (rowVisible[y]) {
        bottom = y;
        gap = 0;
      } else {
        gap++;
      }
    }
    y = bottom + 1;

    colTop.fill(-1);
    for (let row = top; row <= bottom; row++) {
      if (!rowVisible[row]) continue;
      const base = row * width * 4 + 3;
      for (let x = 0; x < width; x++) {
        if (rgba[base + x * 4] !== 0) {
          if (colTop[x] < 0) colTop[x] = row;
          colBottom[x] = row;
        }
      }
    }

    for (let x = 0; x < width;) {
      if (colTop[x] < 0) {
        x++;
        continue;
      }

      const left = x;
      let right = x;
      let minY = colTop[x];
      let maxY = colBottom[x];
      for (let gap = 0; x < width && gap < GAP; x++) {
        if (colTop[x] >= 0) {
          right = x;
          gap = 0;
          if (colTop[x] < minY) minY = colTop[x];
          if (colBottom[x] > maxY) maxY = colBottom[x];
        } else {
          gap++;
        }
      }
      x = right + 1;
      rects.push({ x: left, y: minY, w: right - left + 1, h: maxY - minY + 1 });
    }
  }

  if (rects.length <= MAX_RECTS) return rects;

  // Scattered pixels: one bounding box is cheaper than many headers.
  const x = Math.min(...rects.map((r) => r.x));
  const y = Math.min(...rects.map((r) => r.y));
  const right = Math.max(...rects.map((r) => r.x + r.w));
  const bottom = Math.max(...rects.map((r) => r.y + r.h));
  return [{ x, y, w: right - x, h: bottom - y }];
}

export function crop(rgba, width, rect) {
  const out = Buffer.alloc(rect.w * rect.h * 4);

  for (let row = 0; row < rect.h; row++) {
    const src = ((rect.y + row) * width + rect.x) * 4;
    rgba.copy(out, row * rect.w * 4, src, src + rect.w * 4);
  }

  return out;
}
