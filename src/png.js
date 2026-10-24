// Minimal PNG decoder for Chromium screencast frames: 8-bit RGB or RGBA,
// not interlaced. Returns unpremultiplied RGBA.

import { inflateSync } from "node:zlib";

const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);

  if (pa <= pb && pa <= pc) return a;
  return pb <= pc ? b : c;
}

export function decodePng(buffer) {
  if (buffer.length < 8 || !buffer.subarray(0, 8).equals(SIGNATURE)) {
    throw new Error("not a PNG");
  }

  let offset = 8;
  let width = 0;
  let height = 0;
  let colorType = 0;
  const idat = [];

  while (offset + 8 <= buffer.length) {
    const length = buffer.readUInt32BE(offset);
    const type = buffer.toString("latin1", offset + 4, offset + 8);
    const data = buffer.subarray(offset + 8, offset + 8 + length);
    offset += 12 + length;

    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      const bitDepth = data[8];
      colorType = data[9];
      const interlace = data[12];

      if (bitDepth !== 8 || (colorType !== 2 && colorType !== 6) || interlace !== 0) {
        throw new Error(`unsupported PNG (depth ${bitDepth}, color ${colorType}, interlace ${interlace})`);
      }
    } else if (type === "IDAT") {
      idat.push(data);
    } else if (type === "IEND") {
      break;
    }
  }

  if (!width || !height) throw new Error("PNG without IHDR");

  const channels = colorType === 6 ? 4 : 3;
  const stride = width * channels;
  const raw = inflateSync(Buffer.concat(idat));

  if (raw.length < (stride + 1) * height) throw new Error("truncated PNG data");

  const pixels = Buffer.alloc(stride * height);

  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const src = y * (stride + 1) + 1;
    const dst = y * stride;
    const prev = dst - stride;

    for (let x = 0; x < stride; x++) {
      const value = raw[src + x];
      const left = x >= channels ? pixels[dst + x - channels] : 0;
      const up = y > 0 ? pixels[prev + x] : 0;
      const upLeft = y > 0 && x >= channels ? pixels[prev + x - channels] : 0;
      let out;

      switch (filter) {
        case 0: out = value; break;
        case 1: out = value + left; break;
        case 2: out = value + up; break;
        case 3: out = value + ((left + up) >> 1); break;
        case 4: out = value + paeth(left, up, upLeft); break;
        default: throw new Error(`bad PNG filter ${filter}`);
      }

      pixels[dst + x] = out & 0xff;
    }
  }

  if (channels === 4) return { width, height, rgba: pixels };

  const rgba = Buffer.alloc(width * height * 4);
  for (let i = 0, j = 0; i < pixels.length; i += 3, j += 4) {
    rgba[j] = pixels[i];
    rgba[j + 1] = pixels[i + 1];
    rgba[j + 2] = pixels[i + 2];
    rgba[j + 3] = 255;
  }

  return { width, height, rgba };
}
