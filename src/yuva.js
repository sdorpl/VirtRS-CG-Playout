// Graphics as raw yuva420p frames for ffmpeg (ErsatzTV with the ffmpeg
// overlay, client/vizrs-ffmpeg.py): planes Y, U, V, A, limited range, straight
// alpha as ffmpeg's overlay filters expect. Transparent areas are Y 16,
// U/V 128, A 0. HD uses BT.709, smaller frames BT.601.

const MATRIX = {
  709: [[0.1826, 0.6142, 0.062], [-0.1006, -0.3386, 0.4392], [0.4392, -0.3989, -0.0403]],
  601: [[0.2568, 0.5041, 0.0979], [-0.1482, -0.291, 0.4392], [0.4392, -0.3678, -0.0714]],
};

export function yuvaSize(width, height) {
  return width * height * 2 + Math.ceil(width / 2) * Math.ceil(height / 2) * 2;
}

const blanks = new Map();

/** Fully transparent frame; shared, never written to. */
function blank(width, height) {
  const key = `${width}x${height}`;
  if (!blanks.has(key)) {
    const frame = Buffer.alloc(yuvaSize(width, height));
    const luma = width * height;
    frame.fill(16, 0, luma);
    frame.fill(128, luma, frame.length - luma);
    blanks.set(key, frame);
  }
  return blanks.get(key);
}

/** One yuva420p frame of `width`x`height` from visible rectangles of RGBA. */
export function yuvaFrame(rects, width, height) {
  if (!rects.length) return blank(width, height);

  const out = Buffer.from(blank(width, height));
  const [ky, ku, kv] = MATRIX[height >= 720 ? 709 : 601];
  const cw = Math.ceil(width / 2);
  const uOff = width * height;
  const vOff = uOff + cw * Math.ceil(height / 2);
  const aOff = vOff + cw * Math.ceil(height / 2);

  for (const { x, y, w, h, pixels } of rects) {
    for (let row = 0; row < h; row++) {
      const line = (y + row) * width + x;
      for (let col = 0; col < w; col++) {
        const i = (row * w + col) * 4;
        const a = pixels[i + 3];
        if (a === 0) continue;
        const r = pixels[i];
        const g = pixels[i + 1];
        const b = pixels[i + 2];
        out[line + col] = (16.5 + ky[0] * r + ky[1] * g + ky[2] * b) | 0;
        out[aOff + line + col] = a;
      }
    }

    // Chroma of each 2x2 block, weighted by alpha: no dark or coloured fringes.
    for (let by = y >> 1; by <= (y + h - 1) >> 1; by++) {
      for (let bx = x >> 1; bx <= (x + w - 1) >> 1; bx++) {
        let sa = 0;
        let su = 0;
        let sv = 0;
        for (let dy = 0; dy < 2; dy++) {
          const row = by * 2 + dy - y;
          if (row < 0 || row >= h) continue;
          for (let dx = 0; dx < 2; dx++) {
            const col = bx * 2 + dx - x;
            if (col < 0 || col >= w) continue;
            const i = (row * w + col) * 4;
            const a = pixels[i + 3];
            if (a === 0) continue;
            const r = pixels[i];
            const g = pixels[i + 1];
            const b = pixels[i + 2];
            sa += a;
            su += a * (ku[0] * r + ku[1] * g + ku[2] * b);
            sv += a * (kv[0] * r + kv[1] * g + kv[2] * b);
          }
        }
        if (sa === 0) continue;
        out[uOff + by * cw + bx] = (128.5 + su / sa) | 0;
        out[vOff + by * cw + bx] = (128.5 + sv / sa) | 0;
      }
    }
  }

  return out;
}

/** The frame as yuva420p, built once and kept on the frame like its packet. */
export function framePicture(frame, width, height) {
  if (frame.yuva?.length !== yuvaSize(width, height)) frame.yuva = yuvaFrame(frame.rects, width, height);
  return frame.yuva;
}
