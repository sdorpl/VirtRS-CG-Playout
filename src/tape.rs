// Crawl tape drawn by the sidecar (`tape`, DOKUMENTACJA §1): while a VizRS
// crawl ticker stands still on the page, the page hides its tape and the
// sidecar moves it itself. Chromium films the tape only once every few
// seconds (one window of it, the rest of the page hidden); every frame of the
// channel is the last page picture with the tape blended in at the crawl's
// current offset. The page talks through `window.vizrsTape` of /output
// (VizRS DOKUMENTACJA §10): column X of the crawl window shows point X + s of
// the tape, so a film taken at offset S shifted by s - S is the picture on air.

use std::sync::Arc;

use crate::area::Area;
use crate::json::Value;
use crate::png::Picture;

/// A crawl of the page, as `vizrsTape.state()` reports it after a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Crawl {
    pub id: u64,
    /// The crawl window in page pixels (the render size).
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Offset of the tape in page pixels.
    pub s: f64,
    /// The page hid its tape in this frame: the sidecar draws it.
    pub hidden: bool,
    /// Changes whenever the tape looks different: the films are stale.
    pub key: String,
}

/// The crawls of `vizrsTape.state()`; none when the page has no such interface.
pub fn parse_state(value: &Value) -> Vec<Crawl> {
    let Some(Value::Array(crawls)) = value.get("crawls") else { return Vec::new() };
    crawls
        .iter()
        .filter_map(|c| {
            let number = |key: &str| c.get(key).and_then(Value::as_f64).filter(|v| v.is_finite());
            Some(Crawl {
                id: number("id")? as u64,
                x: number("x")?,
                y: number("y")?,
                w: number("w")?,
                h: number("h")?,
                s: number("s")?,
                hidden: c.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                key: c.get("key").and_then(Value::as_str).unwrap_or("").to_owned(),
            })
        })
        .collect()
}

impl Crawl {
    /// The crawl window in pixels of the area drawn as `region` (page
    /// pixels); None when it is empty or not wholly inside the area.
    pub fn window_in(&self, region: Area) -> Option<Area> {
        let (x0, y0) = (self.x.round(), self.y.round());
        let (x1, y1) = ((self.x + self.w).round(), (self.y + self.h).round());
        let (ax, ay) = (f64::from(region.x), f64::from(region.y));
        let inside = x0 >= ax && y0 >= ay && x1 <= ax + f64::from(region.w) && y1 <= ay + f64::from(region.h);
        (inside && x1 > x0 && y1 > y0).then_some(Area {
            x: (x0 - ax) as u32,
            y: (y0 - ay) as u32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }

    /// The offset on air: whole pixels, so films shift without resampling.
    pub fn offset(&self) -> i64 {
        self.s.round() as i64
    }
}

/// One window of the tape filmed at offset `at`: columns `u0`.. of the tape
/// (u = area column + offset), the rows of the crawl window, straight RGBA.
#[derive(Debug)]
pub struct Film {
    pub at: i64,
    pub u0: i64,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl Film {
    /// The crawl window `window` cut out of a picture of the area taken at offset `at`.
    pub fn cut(picture: &Picture, window: Area, at: i64) -> Option<Film> {
        if window.x + window.w > picture.width || window.y + window.h > picture.height {
            return None;
        }
        let rgba = crate::packets::crop(&picture.rgba, picture.width, (window.x, window.y, window.w, window.h));
        Some(Film { at, u0: i64::from(window.x) + at, w: window.w, h: window.h, rgba })
    }

    fn end(&self) -> i64 {
        self.u0 + i64::from(self.w)
    }
}

/// The films of a crawl: contiguous windows of its tape, oldest first.
#[derive(Default)]
pub struct Reel {
    pub films: Vec<Arc<Film>>,
}

impl Reel {
    /// The offset of the next film the window at offset `s` needs: the
    /// window itself and half a window ahead, so filming stays rare (once per
    /// window of tape). A window outside the reel starts it anew.
    pub fn next_film(&mut self, window: Area, s: i64) -> Option<i64> {
        let start = i64::from(window.x) + s;
        let need = start + i64::from(window.w) + i64::from(window.w / 2);
        self.films.retain(|f| f.end() > start);
        match (self.films.first(), self.films.last()) {
            (Some(first), Some(last)) if first.u0 <= start && last.end() >= start => {
                (last.end() < need).then(|| last.end() - i64::from(window.x))
            }
            _ => {
                self.films.clear();
                Some(s)
            }
        }
    }

    /// True when the films cover the window at offset `s`.
    pub fn covers(&self, window: Area, s: i64) -> bool {
        let (start, end) = (i64::from(window.x) + s, i64::from(window.x + window.w) + s);
        match (self.films.first(), self.films.last()) {
            (Some(first), Some(last)) => first.u0 <= start && last.end() >= end,
            _ => false,
        }
    }
}

/// The picture of an area with the tape blended into the crawl window: the
/// page picture without the tape (`base`) and the films covering offset `s`.
pub struct Composite {
    pub base: Arc<Picture>,
    pub window: Area,
    pub films: Vec<Arc<Film>>,
    pub s: i64,
}

impl Composite {
    pub fn render(&self) -> Picture {
        let (width, height) = (self.base.width, self.base.height);
        let mut rgba = self.base.rgba.clone();
        let window = self.window;
        for film in &self.films {
            // Area columns of the window this film covers at offset s.
            let from = (film.u0 - self.s).max(i64::from(window.x));
            let to = (film.end() - self.s).min(i64::from(window.x + window.w)).min(i64::from(width));
            if from >= to {
                continue;
            }
            for row in 0..window.h.min(film.h) {
                let y = window.y + row;
                if y >= height {
                    break;
                }
                for x in from..to {
                    let j = (x + self.s - film.u0) as usize;
                    let src = &film.rgba[(row as usize * film.w as usize + j) * 4..][..4];
                    let at = (y as usize * width as usize + x as usize) * 4;
                    blend(&mut rgba[at..at + 4], src);
                }
            }
        }
        Picture { width, height, rgba }
    }
}

/// `src` over `dst`, straight alpha (as the compositor puts the tape over the page).
fn blend(dst: &mut [u8], src: &[u8]) {
    let sa = u32::from(src[3]);
    if sa == 0 {
        return;
    }
    if sa == 255 {
        dst.copy_from_slice(src);
        return;
    }
    let da = u32::from(dst[3]) * (255 - sa);
    let oa = sa * 255 + da;
    for c in 0..3 {
        let value = (u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * da + oa / 2) / oa;
        dst[c] = value.min(255) as u8;
    }
    dst[3] = ((oa + 127) / 255).min(255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn film(at: i64, window: Area, value: impl Fn(i64) -> [u8; 4]) -> Arc<Film> {
        let u0 = i64::from(window.x) + at;
        let rgba = (0..window.h).flat_map(|_| (0..i64::from(window.w)).flat_map(|j| value(u0 + j))).collect();
        Arc::new(Film { at, u0, w: window.w, h: window.h, rgba })
    }

    /// The tape: column u is opaque grey u % 256.
    fn grey(u: i64) -> [u8; 4] {
        let v = u.rem_euclid(256) as u8;
        [v, v, v, 255]
    }

    #[test]
    fn the_state_of_the_page_gives_its_crawls() {
        let state =
            crate::json::parse(r#"{"crawls":[{"id":3,"x":10.2,"y":20,"w":300,"h":40,"s":1234.6,"hidden":true,"key":"a"},{"id":"x"}]}"#)
                .unwrap();
        let crawls = parse_state(&state);
        assert_eq!(crawls.len(), 1);
        assert_eq!((crawls[0].id, crawls[0].hidden, crawls[0].offset()), (3, true, 1235));
        assert_eq!(crawls[0].window_in(Area { x: 0, y: 0, w: 640, h: 80 }), Some(Area { x: 10, y: 20, w: 300, h: 40 }));
        assert_eq!(crawls[0].window_in(Area { x: 0, y: 30, w: 640, h: 80 }), None);
        assert!(parse_state(&Value::Null).is_empty());
    }

    #[test]
    fn films_follow_the_window_contiguously() {
        let window = Area { x: 100, y: 0, w: 200, h: 1 };
        let mut reel = Reel::default();
        assert_eq!(reel.next_film(window, 50), Some(50));
        reel.films.push(film(50, window, grey));
        // 150..350 filmed; the window at 50 needs up to 450: the next window.
        assert_eq!(reel.next_film(window, 50), Some(250));
        reel.films.push(film(250, window, grey));
        assert_eq!(reel.next_film(window, 60), None);
        assert!(reel.covers(window, 60));
        // Further on, the first film falls behind and goes.
        assert_eq!(reel.next_film(window, 260), Some(450));
        assert_eq!(reel.films.len(), 1);
        // A jump outside the reel starts it anew.
        assert_eq!(reel.next_film(window, 5000), Some(5000));
        assert!(reel.films.is_empty());
    }

    #[test]
    fn the_composite_shifts_the_films_over_the_page() {
        let window = Area { x: 2, y: 1, w: 6, h: 1 };
        // Page 10x3: transparent, a half-transparent red pixel in the window.
        let mut base = vec![0u8; 10 * 3 * 4];
        base[(10 + 5) * 4..][..4].copy_from_slice(&[255, 0, 0, 128]);
        let base = Arc::new(Picture { width: 10, height: 3, rgba: base });
        // Tape transparent except columns 20..23, white at half alpha.
        let tape = |u: i64| if (20..23).contains(&u) { [255, 255, 255, 128] } else { [0, 0, 0, 0] };
        let films = vec![film(10, window, tape), film(16, window, tape)];
        let picture = Composite { base, window, films, s: 15 }.render();
        let px = |x: usize, y: usize| <[u8; 4]>::try_from(&picture.rgba[(y * 10 + x) * 4..][..4]).unwrap();
        // Columns 5..7 show tape 20..22; column 5 blends over the red pixel.
        assert_eq!(px(4, 1), [0, 0, 0, 0]);
        assert_eq!(px(6, 1), [255, 255, 255, 128]);
        let over = px(5, 1);
        assert_eq!(over[3], 192);
        assert_eq!(over, [255, 170, 170, 192]);
        // Outside the window nothing changes.
        assert_eq!(px(5, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn the_composite_equals_the_tape_at_any_offset() {
        let window = Area { x: 3, y: 0, w: 8, h: 2 };
        let base = Arc::new(Picture { width: 12, height: 2, rgba: vec![0; 12 * 2 * 4] });
        let films = vec![film(0, window, grey), film(8, window, grey)];
        for s in 0..=8 {
            let picture = Composite { base: Arc::clone(&base), window, films: films.clone(), s }.render();
            for x in 3..11 {
                assert_eq!(picture.rgba[(12 + x) * 4], grey(x as i64 + s)[0], "s {s} x {x}");
            }
        }
    }
}
