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

/** One rectangle of RGBA/BGRA pixels; the rest of the canvas is transparent. */
export function rectanglePacket(rect, pixels) {
  if (pixels.length !== rect.w * rect.h * 4) throw new Error("rectangle size mismatch");

  const meta = Buffer.alloc(10);
  meta.writeUInt16BE(1, 0);
  meta.writeUInt16BE(rect.x, 2);
  meta.writeUInt16BE(rect.y, 4);
  meta.writeUInt16BE(rect.w, 6);
  meta.writeUInt16BE(rect.h, 8);
  return Buffer.concat([header(Type.Rectangles, meta.length + pixels.length), meta, pixels]);
}

/** Packet for a captured frame: the visible rectangle, or Clear. */
export function framePacket(frame) {
  return frame.rect ? rectanglePacket(frame.rect, frame.pixels) : clearPacket();
}

/** Bounding box of the pixels with alpha > 0, or null for a transparent frame. */
export function visibleRect(rgba, width, height) {
  let minX = width;
  let minY = height;
  let maxX = -1;
  let maxY = -1;

  for (let y = 0; y < height; y++) {
    const row = y * width * 4;

    for (let x = 0; x < width; x++) {
      if (rgba[row + x * 4 + 3] !== 0) {
        if (x < minX) minX = x;
        if (x > maxX) maxX = x;
        if (y < minY) minY = y;
        maxY = y;
      }
    }
  }

  return maxX < 0 ? null : { x: minX, y: minY, w: maxX - minX + 1, h: maxY - minY + 1 };
}

export function crop(rgba, width, rect) {
  const out = Buffer.alloc(rect.w * rect.h * 4);

  for (let row = 0; row < rect.h; row++) {
    const src = ((rect.y + row) * width + rect.x) * 4;
    rgba.copy(out, row * rect.w * 4, src, src + rect.w * 4);
  }

  return out;
}
