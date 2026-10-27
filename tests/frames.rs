// Rectangles, yuva420p and PNG as the Node.js build gives them
// (tests/vectors/frames.json, png.json, server.json).

mod common;

use virtrs_cg::json::Value;
use virtrs_cg::packets::Rect;
use virtrs_cg::png::decode_png;
use virtrs_cg::rects::visible_rects;
use virtrs_cg::yuva::{yuva_area, yuva_frame, yuva_size};

use common::{assert_bytes, base64, num, paint, vectors};

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).unwrap_or_else(|| panic!("{key}"))
}

fn boxes(v: &Value) -> Vec<(u32, u32, u32, u32)> {
    list(v, "rects").iter().map(|r| (num(r, "x"), num(r, "y"), num(r, "w"), num(r, "h"))).collect()
}

#[test]
fn visible_rects_match_node() {
    let v = vectors("frames");
    for case in list(&v, "rects") {
        let (width, height, rgba) = paint(case.get("picture").unwrap());
        assert_eq!(visible_rects(&rgba, width, height), boxes(case), "{}", case.get("name").unwrap());
    }
}

#[test]
fn yuva_frames_match_node() {
    let v = vectors("frames");
    let pictures = list(&v, "rects");
    for case in list(&v, "yuva") {
        let name = case.get("name").unwrap().to_string();
        let (rects, width, height) = match case.get("picture").and_then(Value::as_str) {
            Some(picture) => {
                let source = pictures.iter().find(|p| p.get("name").and_then(Value::as_str) == Some(picture)).unwrap();
                let (width, height, rgba) = paint(source.get("picture").unwrap());
                let rects = boxes(source)
                    .into_iter()
                    .map(|(x, y, w, h)| Rect { x, y, w, h, pixels: virtrs_cg::packets::crop(&rgba, width, (x, y, w, h)) })
                    .collect::<Vec<_>>();
                (rects, width, height)
            }
            None => {
                let rects = list(case, "rects")
                    .iter()
                    .map(|r| Rect {
                        x: num(r, "x"),
                        y: num(r, "y"),
                        w: num(r, "w"),
                        h: num(r, "h"),
                        pixels: base64(r.get("pixels_b64").and_then(Value::as_str).unwrap()),
                    })
                    .collect();
                (rects, num(case, "width"), num(case, "height"))
            }
        };
        assert_eq!(yuva_size(width, height), num(case, "size") as usize, "{name}");
        assert_bytes(&yuva_frame(&rects, width, height), case.get("frame").unwrap(), &name);
    }
}

/// A frame of a graphics area (even position) is that part of the channel's
/// frame, plane by plane, also for rectangles that reach out of it.
#[test]
fn yuva_of_an_area_is_the_part_of_the_whole_frame() {
    let (width, height) = (40u32, 30u32);
    let rect = |x: u32, y: u32, w: u32, h: u32, seed: u32| Rect {
        x,
        y,
        w,
        h,
        pixels: (0..w * h * 4).map(|i| ((i * 37 + seed * 11) % 256) as u8).collect(),
    };
    let rects = [rect(3, 5, 11, 9, 1), rect(17, 13, 20, 15, 2), rect(0, 0, 4, 3, 3)];
    let whole = yuva_frame(&rects, width, height);
    let (ax, ay, aw, ah) = (8u32, 6u32, 22u32, 18u32);
    let part = yuva_area(&rects, (ax, ay, aw, ah), false);
    assert_eq!(part.len(), yuva_size(aw, ah));

    // Planes: Y and A full size, U and V halved.
    let planes = |w: u32, h: u32| {
        let (luma, chroma) = ((w * h) as usize, (w.div_ceil(2) * h.div_ceil(2)) as usize);
        [(0, w, 1), (luma, w.div_ceil(2), 2), (luma + chroma, w.div_ceil(2), 2), (luma + 2 * chroma, w, 1)]
    };
    for ((whole_at, whole_w, sub), (part_at, part_w, _)) in planes(width, height).into_iter().zip(planes(aw, ah)) {
        for row in 0..ah / sub {
            for col in 0..aw / sub {
                let expected = whole[whole_at + ((ay / sub + row) * whole_w + ax / sub + col) as usize];
                assert_eq!(part[part_at + (row * part_w + col) as usize], expected, "plane at {part_at}, {col},{row}");
            }
        }
    }
    // Outside every rectangle: transparent.
    assert_eq!(yuva_area(&rects, (0, 20, 2, 2), true)[..4], [16, 16, 16, 16]);
}

#[test]
fn png_decoding_matches_node() {
    let v = vectors("png");
    for case in list(&v, "cases") {
        let picture = decode_png(&base64(case.get("png_b64").and_then(Value::as_str).unwrap())).unwrap();
        assert_eq!((picture.width, picture.height), (num(case, "width"), num(case, "height")));
        assert_eq!(picture.rgba, base64(case.get("rgba_b64").and_then(Value::as_str).unwrap()), "{case}");
    }
    assert!(decode_png(b"not a png").is_err());
}

#[test]
fn rates_and_screencast_steps_match_node() {
    let v = vectors("server");
    for case in list(&v, "rates") {
        let input = case.get("input").and_then(Value::as_str);
        assert_eq!(Value::from(virtrs_cg::server::parse_rate(input)), *case.get("fps").unwrap(), "{case}");
    }
    for case in list(&v, "every_nth_frame") {
        let fps = case.get("fps").and_then(Value::as_f64).unwrap();
        assert_eq!(virtrs_cg::capture::every_nth_frame(fps), num(case, "n"), "{case}");
    }
}
