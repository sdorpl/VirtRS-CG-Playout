// Graphics area: the part of the channel the graphics occupy, e.g. a ticker
// `1920x140+0+940` (ImageMagick geometry). Chromium then draws only that part
// of /output (the page still lays out at full size), the sidecar decodes and
// converts only it, and the ffmpeg overlay gets frames of its size, overlaid
// at its position. Everything outside the area is cut off.

/// A rectangle of the channel in pixels; even, so 4:2:0 chroma stays aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Area {
    /// `""` or `full` = the whole channel (None); `WxH+X+Y` (`+X+Y` may be
    /// left out) in channel pixels. Clamped to the channel and made even:
    /// the position down, the size up. A space stands for `+` (an unencoded
    /// `+` in a URL query arrives as one).
    pub fn parse(spec: &str, width: u32, height: u32) -> Result<Option<Area>, String> {
        let value = spec.trim().to_ascii_lowercase().replace(' ', "+");
        if value.is_empty() || value == "full" {
            return Ok(None);
        }
        let bad = || format!("bad graphics area \"{spec}\" (full or WxH+X+Y, e.g. 1920x140+0+940)");
        let number =
            |text: &str| (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse::<u32>().ok()).flatten();
        let (size, position) = value.split_once('+').map_or((value.as_str(), "0+0"), |(size, position)| (size, position));
        let (w, h) = size.split_once('x').and_then(|(w, h)| Some((number(w)?, number(h)?))).ok_or_else(bad)?;
        let (x, y) = position.split_once('+').and_then(|(x, y)| Some((number(x)?, number(y)?))).ok_or_else(bad)?;

        let (x0, y0) = (x.min(width) & !1, y.min(height) & !1);
        let x1 = x.saturating_add(w).min(width).saturating_add(1) & !1;
        let y1 = y.saturating_add(h).min(height).saturating_add(1) & !1;
        let (x1, y1) = (x1.min(width), y1.min(height));
        if x1 <= x0 || y1 <= y0 || w == 0 || h == 0 {
            return Err(format!("graphics area \"{spec}\" is outside the {width}x{height} channel"));
        }
        let area = Area { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
        Ok((area != Area { x: 0, y: 0, w: width, h: height }).then_some(area))
    }

    /// The same part of a picture of `to` instead of `from` (render size).
    pub fn scaled(self, from: (u32, u32), to: (u32, u32)) -> Area {
        if from == to {
            return self;
        }
        let edge = |v: u32, f: u32, t: u32| (f64::from(v) * f64::from(t) / f64::from(f)).round() as u32;
        let (x, y) = (edge(self.x, from.0, to.0), edge(self.y, from.1, to.1));
        let (x1, y1) = (edge(self.x + self.w, from.0, to.0).min(to.0), edge(self.y + self.h, from.1, to.1).min(to.1));
        Area { x, y, w: x1.saturating_sub(x).max(1), h: y1.saturating_sub(y).max(1) }
    }

    /// `WxH+X+Y`, as `/health` shows it.
    pub fn geometry(self) -> String {
        format!("{}x{}+{}+{}", self.w, self.h, self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_full_and_errors() {
        let area = |spec| Area::parse(spec, 1920, 1080);
        assert_eq!(area(""), Ok(None));
        assert_eq!(area(" Full "), Ok(None));
        assert_eq!(area("1920x1080"), Ok(None));
        assert_eq!(area("1920x140+0+940"), Ok(Some(Area { x: 0, y: 940, w: 1920, h: 140 })));
        assert_eq!(area("400X100"), Ok(Some(Area { x: 0, y: 0, w: 400, h: 100 })));
        assert_eq!(area("1920x140 0 940"), area("1920x140+0+940"));
        // Even, inside the channel.
        assert_eq!(area("301x101+11+977"), Ok(Some(Area { x: 10, y: 976, w: 302, h: 102 })));
        assert_eq!(area("500x500+1700+900"), Ok(Some(Area { x: 1700, y: 900, w: 220, h: 180 })));
        for bad in ["1920", "axb", "10x10+1", "10x10-5-5", "0x10", "10x10+1920+0"] {
            assert!(area(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scaled_to_the_render_size() {
        let ticker = Area { x: 0, y: 940, w: 1920, h: 140 };
        assert_eq!(ticker.scaled((1920, 1080), (1920, 1080)), ticker);
        assert_eq!(ticker.scaled((1920, 1080), (1280, 720)), Area { x: 0, y: 627, w: 1280, h: 93 });
        assert_eq!(ticker.geometry(), "1920x140+0+940");
    }
}
