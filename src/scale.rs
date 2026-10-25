// Graphics resolution: Chromium may render /output smaller (or larger) than
// the channel. ErsatzTV always draws on a canvas of the channel size, so the
// visible rectangles are scaled to it here, as libvips did for the Node.js
// build (`sharp` resize): alpha premultiplied, Lanczos3 when shrinking,
// bicubic when enlarging, pixel centres aligned.

use crate::packets::{Rect, crop};

// Transparent margin around each rectangle, so the resampling kernel sees
// the real edge instead of a repeated border pixel.
const PAD: u32 = 4;

/// JavaScript's `Math.round`: halves go up.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Render size for a channel: "" or "auto" = the channel size, "720" or
/// "720p" = that height in the channel's proportions, "1280x720" = exact.
pub fn render_size(spec: &str, width: u32, height: u32) -> Result<(u32, u32), String> {
    let value = spec.trim().to_lowercase();
    if value.is_empty() || value == "auto" {
        return Ok((width, height));
    }

    let number = |text: &str| (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse::<f64>().ok()).flatten();
    let (w, h) = if let Some((w, h)) = value.split_once('x').and_then(|(w, h)| Some((number(w)?, number(h)?))) {
        (w, h)
    } else if let Some(h) = number(value.strip_suffix('p').unwrap_or(&value)) {
        (js_round(f64::from(width) * h / f64::from(height) / 2.0) * 2.0, h)
    } else {
        return Err(format!("bad render size \"{spec}\" (auto, 720p or 1280x720)"));
    };

    if !(16.0..=7680.0).contains(&w) || !(16.0..=4320.0).contains(&h) {
        return Err(format!("render size {w}x{h} out of range"));
    }
    Ok((w as u32, h as u32))
}

fn lanczos3(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    if x.abs() >= 3.0 {
        return 0.0;
    }
    let px = std::f64::consts::PI * x;
    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
}

/// Catmull-Rom, libvips' "bicubic".
fn cubic(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        1.5 * x * x * x - 2.5 * x * x + 1.0
    } else if x < 2.0 {
        -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
    } else {
        0.0
    }
}

/// Source taps and their weights for every output position of one axis.
fn weights(src: usize, dst: usize) -> Vec<(usize, Vec<f64>)> {
    let scale = dst as f64 / src as f64;
    // Shrinking stretches the kernel over the source pixels it covers.
    let (kernel, support, stretch): (fn(f64) -> f64, f64, f64) =
        if scale < 1.0 { (lanczos3, 3.0 / scale, 1.0 / scale) } else { (cubic, 2.0, 1.0) };

    (0..dst)
        .map(|out| {
            // libvips centres pixels when shrinking; enlarging (vips_affine) samples at out / scale - 0.5.
            let centre = if scale < 1.0 { (out as f64 + 0.5) / scale - 0.5 } else { out as f64 / scale - 0.5 };
            let first = (centre - support).ceil() as i64;
            let last = (centre + support).floor() as i64;
            let mut taps: Vec<(i64, f64)> = (first..=last).map(|i| (i, kernel((i as f64 - centre) / stretch))).collect();
            let sum: f64 = taps.iter().map(|(_, w)| w).sum();
            for tap in &mut taps {
                tap.1 /= sum;
            }
            // Pixels past the edge repeat the edge pixel.
            let start = first.clamp(0, src as i64 - 1) as usize;
            let mut folded = vec![0.0; (last.clamp(0, src as i64 - 1) as usize) - start + 1];
            for (i, w) in taps {
                folded[i.clamp(0, src as i64 - 1) as usize - start] += w;
            }
            (start, folded)
        })
        .collect()
}

/// One axis of a premultiplied 4-channel image: `count` lines of `src` samples
/// (`stride` apart within a line, `line` apart between lines) to `dst` samples.
fn pass(input: &[f64], src: usize, dst: usize, count: usize, along: impl Fn(usize, usize) -> usize, quantize: bool) -> Vec<f64> {
    let taps = weights(src, dst);
    let mut out = vec![0.0; dst * count * 4];
    for line in 0..count {
        for (o, (start, w)) in taps.iter().enumerate() {
            for c in 0..4 {
                let value: f64 = w.iter().enumerate().map(|(k, w)| w * input[along(line, start + k) * 4 + c]).sum();
                // libvips reduces 8-bit images in 8 bits: each pass rounds and clips.
                out[(line * dst + o) * 4 + c] = if quantize { js_round(value).clamp(0.0, 255.0) } else { value };
            }
        }
    }
    out
}

/// RGBA `sw`x`sh` resized to `dw`x`dh`, alpha premultiplied while resampling.
/// Shrinking goes as `vips_reduce` (vertical, then horizontal, each pass in 8 bits),
/// enlarging as one bicubic step without rounding in between.
pub fn resize(rgba: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let premultiplied: Vec<f64> = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = f64::from(p[3]) / 255.0;
            [f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, f64::from(p[3])]
        })
        .collect();

    let shrink = dw < sw || dh < sh;
    let result = if shrink {
        // Vertical first: `count` columns of `sh` samples; the result is stored column-major.
        let columns = pass(&premultiplied, sh, dh, sw, |x, y| y * sw + x, true);
        // Then horizontal: `dh` rows of `sw` samples from the column-major buffer.
        pass(&columns, sw, dw, dh, |y, x| x * dh + y, true)
    } else {
        let rows = pass(&premultiplied, sw, dw, sh, |y, x| y * sw + x, false);
        // Columns of the row result; the output is column-major, transposed below.
        let cols = pass(&rows, sh, dh, dw, |x, y| y * dw + x, false);
        let mut out = vec![0.0; dw * dh * 4];
        for x in 0..dw {
            for y in 0..dh {
                out[(y * dw + x) * 4..(y * dw + x) * 4 + 4].copy_from_slice(&cols[(x * dh + y) * 4..(x * dh + y) * 4 + 4]);
            }
        }
        out
    };

    let mut out = vec![0u8; dw * dh * 4];
    for (pixel, p) in out.chunks_exact_mut(4).zip(result.chunks_exact(4)) {
        let a = p[3].clamp(0.0, 255.0);
        let alpha = js_round(a) as u8;
        // A pixel that ends up transparent carries no colour, as from libvips.
        if alpha > 0 {
            for c in 0..3 {
                pixel[c] = js_round(p[c] * 255.0 / a).clamp(0.0, 255.0) as u8;
            }
        }
        pixel[3] = alpha;
    }
    out
}

/// Rectangles of a `width`x`height` picture, scaled with their pixels to `out_width`x`out_height`.
pub fn scale_rects(rgba: &[u8], width: u32, height: u32, rects: &[(u32, u32, u32, u32)], out_width: u32, out_height: u32) -> Vec<Rect> {
    let sx = f64::from(out_width) / f64::from(width);
    let sy = f64::from(out_height) / f64::from(height);

    rects
        .iter()
        .map(|&(rx, ry, rw, rh)| {
            let (x0, y0) = (rx.saturating_sub(PAD), ry.saturating_sub(PAD));
            let (x1, y1) = ((rx + rw + PAD).min(width), (ry + rh + PAD).min(height));
            let x = (f64::from(x0) * sx).floor() as u32;
            let y = (f64::from(y0) * sy).floor() as u32;
            let w = ((f64::from(x1) * sx).ceil() as u32).min(out_width) - x;
            let h = ((f64::from(y1) * sy).ceil() as u32).min(out_height) - y;
            let src = crop(rgba, width, (x0, y0, x1 - x0, y1 - y0));
            let pixels = resize(&src, (x1 - x0) as usize, (y1 - y0) as usize, w as usize, h as usize);
            Rect { x, y, w, h, pixels }
        })
        .collect()
}
