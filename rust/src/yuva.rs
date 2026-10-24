// Graphics as raw yuva420p frames for ffmpeg (ErsatzTV with the ffmpeg
// overlay, client/vizrs-ffmpeg.py): planes Y, U, V, A, limited range, straight
// alpha as ffmpeg's overlay filters expect. Transparent areas are Y 16,
// U/V 128, A 0. HD uses BT.709, smaller frames BT.601. The arithmetic follows
// src/yuva.js step by step (doubles, truncation), so frames match byte for byte.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::packets::Rect;

type Matrix = [[f64; 3]; 3];
const BT709: Matrix = [[0.1826, 0.6142, 0.062], [-0.1006, -0.3386, 0.4392], [0.4392, -0.3989, -0.0403]];
const BT601: Matrix = [[0.2568, 0.5041, 0.0979], [-0.1482, -0.291, 0.4392], [0.4392, -0.3678, -0.0714]];

pub fn yuva_size(width: u32, height: u32) -> usize {
    let (w, h) = (width as usize, height as usize);
    w * h * 2 + w.div_ceil(2) * h.div_ceil(2) * 2
}

/// Fully transparent frames by size.
type Blanks = Mutex<HashMap<(u32, u32), Arc<[u8]>>>;

/// Fully transparent frame; shared, never written to.
fn blank(width: u32, height: u32) -> Arc<[u8]> {
    static BLANKS: OnceLock<Blanks> = OnceLock::new();
    let mut blanks = BLANKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(blanks.entry((width, height)).or_insert_with(|| {
        let mut frame = vec![0u8; yuva_size(width, height)];
        let luma = (width * height) as usize;
        let alpha = frame.len() - luma;
        frame[..luma].fill(16);
        frame[luma..alpha].fill(128);
        frame.into()
    }))
}

/// JavaScript's `x | 0` for the values here (positive, far below 2^31).
fn trunc(x: f64) -> u8 {
    x as i32 as u8
}

/// One yuva420p frame of `width`x`height` from visible rectangles of RGBA.
pub fn yuva_frame(rects: &[Rect], width: u32, height: u32) -> Arc<[u8]> {
    if rects.is_empty() {
        return blank(width, height);
    }

    let mut out = blank(width, height).to_vec();
    let [ky, ku, kv] = if height >= 720 { BT709 } else { BT601 };
    let w = width as usize;
    let cw = w.div_ceil(2);
    let ch = (height as usize).div_ceil(2);
    let u_off = w * height as usize;
    let v_off = u_off + cw * ch;
    let a_off = v_off + cw * ch;

    for rect in rects {
        let (x, y, rw, rh) = (rect.x as usize, rect.y as usize, rect.w as usize, rect.h as usize);
        let pixels = &rect.pixels;

        for row in 0..rh {
            let line = (y + row) * w + x;
            for col in 0..rw {
                let i = (row * rw + col) * 4;
                let a = pixels[i + 3];
                if a == 0 {
                    continue;
                }
                let (r, g, b) = (f64::from(pixels[i]), f64::from(pixels[i + 1]), f64::from(pixels[i + 2]));
                out[line + col] = trunc(16.5 + ky[0] * r + ky[1] * g + ky[2] * b);
                out[a_off + line + col] = a;
            }
        }

        // Chroma of each 2x2 block, weighted by alpha: no dark or coloured fringes.
        for by in y >> 1..=(y + rh - 1) >> 1 {
            for bx in x >> 1..=(x + rw - 1) >> 1 {
                let (mut sa, mut su, mut sv) = (0.0, 0.0, 0.0);
                for dy in 0..2 {
                    let Some(row) = (by * 2 + dy).checked_sub(y).filter(|&r| r < rh) else { continue };
                    for dx in 0..2 {
                        let Some(col) = (bx * 2 + dx).checked_sub(x).filter(|&c| c < rw) else { continue };
                        let i = (row * rw + col) * 4;
                        let a = f64::from(pixels[i + 3]);
                        if a == 0.0 {
                            continue;
                        }
                        let (r, g, b) = (f64::from(pixels[i]), f64::from(pixels[i + 1]), f64::from(pixels[i + 2]));
                        sa += a;
                        su += a * (ku[0] * r + ku[1] * g + ku[2] * b);
                        sv += a * (kv[0] * r + kv[1] * g + kv[2] * b);
                    }
                }
                if sa == 0.0 {
                    continue;
                }
                out[u_off + by * cw + bx] = trunc(128.5 + su / sa);
                out[v_off + by * cw + bx] = trunc(128.5 + sv / sa);
            }
        }
    }

    out.into()
}
