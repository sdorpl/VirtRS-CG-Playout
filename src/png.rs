// PNG frames from the Chromium screencast to straight (unpremultiplied) RGBA,
// as `sharp(png).ensureAlpha().raw()` gave them to the Node.js build.

use std::io::Cursor;

use png::{ColorType, Decoder, Transformations};

pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn decode_png(data: &[u8]) -> Result<Picture, String> {
    let mut decoder = Decoder::new(Cursor::new(data));
    // Palette, low bit depths and 16 bits to 8-bit gray or colour.
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("PNG: {e}"))?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("PNG: image too large")?];
    let info = reader.next_frame(&mut buffer).map_err(|e| format!("PNG: {e}"))?;
    buffer.truncate(info.buffer_size());

    let rgba = match info.color_type {
        ColorType::Rgba => buffer,
        ColorType::Rgb => buffer.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        ColorType::GrayscaleAlpha => buffer.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        ColorType::Grayscale => buffer.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        ColorType::Indexed => return Err("PNG: palette not expanded".into()),
    };
    Ok(Picture { width: info.width, height: info.height, rgba })
}
