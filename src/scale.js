// Graphics resolution: Chromium may render /output smaller (or larger) than
// the channel. ErsatzTV always draws on a canvas of the channel size, so the
// visible rectangles are scaled to it here (libvips through `sharp`).

import { crop } from "./packets.js";

const sharp = await import("sharp").then((m) => m.default, () => null);

/** Scaling needs `sharp`; without it the graphics render at the channel size. */
export const canScale = Boolean(sharp);

// Transparent margin around each rectangle, so the resampling kernel sees
// the real edge instead of a repeated border pixel.
const PAD = 4;

/**
 * Render size for a channel: "" or "auto" = the channel size, "720" or "720p"
 * = that height in the channel's proportions, "1280x720" = exact.
 */
export function renderSize(spec, width, height) {
  const value = String(spec ?? "").trim().toLowerCase();
  if (!value || value === "auto") return { width, height };

  let w;
  let h;
  let m = value.match(/^(\d+)x(\d+)$/);
  if (m) {
    w = Number(m[1]);
    h = Number(m[2]);
  } else if ((m = value.match(/^(\d+)p?$/))) {
    h = Number(m[1]);
    w = Math.round((width * h) / height / 2) * 2;
  } else {
    throw new Error(`bad render size "${spec}" (auto, 720p or 1280x720)`);
  }

  if (w < 16 || h < 16 || w > 7680 || h > 4320) throw new Error(`render size ${w}x${h} out of range`);
  return { width: w, height: h };
}

/** Rectangles of a `width`x`height` frame, scaled with their pixels to `outWidth`x`outHeight`. */
export function scaleRects(rgba, width, height, rects, outWidth, outHeight) {
  const sx = outWidth / width;
  const sy = outHeight / height;

  return Promise.all(rects.map(async (rect) => {
    const x0 = Math.max(0, rect.x - PAD);
    const y0 = Math.max(0, rect.y - PAD);
    const x1 = Math.min(width, rect.x + rect.w + PAD);
    const y1 = Math.min(height, rect.y + rect.h + PAD);
    const x = Math.floor(x0 * sx);
    const y = Math.floor(y0 * sy);
    const w = Math.min(outWidth, Math.ceil(x1 * sx)) - x;
    const h = Math.min(outHeight, Math.ceil(y1 * sy)) - y;

    const src = crop(rgba, width, { x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
    // libvips premultiplies alpha for the resize: no dark fringes.
    const pixels = await sharp(src, { raw: { width: x1 - x0, height: y1 - y0, channels: 4 } })
      .resize(w, h, { fit: "fill" })
      .raw()
      .toBuffer();
    return { x, y, w, h, pixels };
  }));
}
