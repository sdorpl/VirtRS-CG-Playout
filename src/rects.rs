// Visible rectangles of a picture (`visibleRects` of the Node.js sidecar): boxes
// around the pixels with alpha > 0, as bands of rows split into column runs,
// each trimmed to its own rows. A logo in one corner and a ticker at the
// bottom become two small rectangles instead of one box over the whole frame.

// Transparent gaps narrower than this stay inside one rectangle: fewer
// rectangles, a few more transparent pixels.
const GAP: u32 = 32;
const MAX_RECTS: usize = 32;

/// `(x, y, w, h)` boxes of an RGBA picture.
pub fn visible_rects(rgba: &[u8], width: u32, height: u32) -> Vec<(u32, u32, u32, u32)> {
    let (w, h) = (width as usize, height as usize);
    let row_bytes = w * 4;
    // Transparent pixels from Chromium are all zero: skip empty rows fast,
    // look at alpha only where something may be drawn.
    let row_visible: Vec<bool> = (0..h)
        .map(|y| {
            let row = &rgba[y * row_bytes..(y + 1) * row_bytes];
            !row.iter().all(|&b| b == 0) && row.chunks_exact(4).any(|p| p[3] != 0)
        })
        .collect();

    let mut rects = Vec::new();
    let mut col_top = vec![-1i64; w];
    let mut col_bottom = vec![0i64; w];
    let mut y = 0usize;

    while y < h {
        if !row_visible[y] {
            y += 1;
            continue;
        }

        // One band: visible rows with gaps shorter than GAP.
        let top = y;
        let mut bottom = y;
        let mut gap = 0;
        while y < h && gap < GAP {
            if row_visible[y] {
                bottom = y;
                gap = 0;
            } else {
                gap += 1;
            }
            y += 1;
        }
        y = bottom + 1;

        col_top.fill(-1);
        for (row, _) in row_visible.iter().enumerate().take(bottom + 1).skip(top).filter(|(_, visible)| **visible) {
            let base = row * row_bytes + 3;
            for x in 0..w {
                if rgba[base + x * 4] != 0 {
                    if col_top[x] < 0 {
                        col_top[x] = row as i64;
                    }
                    col_bottom[x] = row as i64;
                }
            }
        }

        let mut x = 0usize;
        while x < w {
            if col_top[x] < 0 {
                x += 1;
                continue;
            }
            let left = x;
            let mut right = x;
            let (mut min_y, mut max_y) = (col_top[x], col_bottom[x]);
            let mut gap = 0;
            while x < w && gap < GAP {
                if col_top[x] >= 0 {
                    right = x;
                    gap = 0;
                    min_y = min_y.min(col_top[x]);
                    max_y = max_y.max(col_bottom[x]);
                } else {
                    gap += 1;
                }
                x += 1;
            }
            x = right + 1;
            rects.push((left as u32, min_y as u32, (right - left + 1) as u32, (max_y - min_y + 1) as u32));
        }
    }

    if rects.len() <= MAX_RECTS {
        return rects;
    }

    // Scattered pixels: one bounding box is cheaper than many headers.
    let x0 = rects.iter().map(|r| r.0).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r.1).min().unwrap_or(0);
    let x1 = rects.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
    vec![(x0, y0, x1 - x0, y1 - y0)]
}
