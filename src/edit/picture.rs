//! Image compression. Everything leaves as a PNG: lossless by default, with a reduced palette
//! when fewer colours are asked for.

use std::collections::HashMap;

use image::RgbaImage;

/// How long the encoder spends looking for a smaller file. Never changes a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Fast,
    Balanced,
    Best,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageEdit {
    /// The palette size, or `None` to keep every colour.
    pub colors: Option<u16>,
    pub dither: bool,
    pub effort: Effort,
    /// Inverts green, converting a normal map between the DirectX and OpenGL conventions.
    pub flip_green: bool,
}

impl Default for ImageEdit {
    fn default() -> Self {
        ImageEdit { colors: None, dither: true, effort: Effort::Balanced, flip_green: false }
    }
}

/// Inverts the green channel of straight RGBA bytes.
pub fn invert_green(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) { pixel[1] = 255 - pixel[1]; }
}

/// The PNG, and the pixels it decodes to.
pub fn encode(source: &RgbaImage, edit: ImageEdit) -> Result<(Vec<u8>, RgbaImage), String> {
    let Some(limit) = edit.colors else {
        return Ok((write_lossless(source, edit.effort)?, source.clone()));
    };
    let limit = usize::from(limit).clamp(2, 256);
    if let Some(palette) = exact_palette(source, limit) {
        let indices = source.pixels().map(|p| palette[&p.0]).collect();
        let mut colors = vec![[0u8; 4]; palette.len()];
        for (color, index) in &palette { colors[usize::from(*index)] = *color; }
        return Ok((write_indexed(source.width(), source.height(), &colors, indices, edit.effort)?, source.clone()));
    }
    let mut out = source.clone();
    // Sampling every pixel of a small image; a large one would only take longer to say the same.
    let sampling = if source.len() < 1 << 20 { 1 } else { 10 };
    let quantizer = color_quant::NeuQuant::new(sampling, limit, source);
    if edit.dither {
        image::imageops::dither(&mut out, &quantizer);
    } else {
        for pixel in out.pixels_mut() { quantizer.map_pixel(&mut pixel.0); }
    }
    let colors: Vec<[u8; 4]> = quantizer.color_map_rgba().chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    let indices = out.pixels().map(|p| quantizer.index_of(&p.0) as u8).collect();
    Ok((write_indexed(source.width(), source.height(), &colors, indices, edit.effort)?, out))
}

/// Every colour in `image`, numbered, if it fits in `limit` of them.
fn exact_palette(image: &RgbaImage, limit: usize) -> Option<HashMap<[u8; 4], u8>> {
    let mut seen = HashMap::with_capacity(limit + 1);
    for pixel in image.pixels() {
        let next = seen.len();
        seen.entry(pixel.0).or_insert_with(|| next as u8);
        if seen.len() > limit { return None }
    }
    Some(seen)
}

fn compression(effort: Effort) -> png::Compression {
    match effort {
        Effort::Fast => png::Compression::Fast,
        Effort::Balanced => png::Compression::Balanced,
        Effort::Best => png::Compression::High,
    }
}

fn finish(encoder: png::Encoder<'_, &mut Vec<u8>>, data: &[u8]) -> Result<(), String> {
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(data).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

/// True colour, in the narrowest channel layout that loses nothing.
fn write_lossless(image: &RgbaImage, effort: Effort) -> Result<Vec<u8>, String> {
    let raw = image.as_raw();
    let opaque = raw.chunks_exact(4).all(|p| p[3] == 255);
    let gray = raw.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]);
    let (color, data): (png::ColorType, Vec<u8>) = match (gray, opaque) {
        (true, true) => (png::ColorType::Grayscale, raw.chunks_exact(4).map(|p| p[0]).collect()),
        (true, false) => (png::ColorType::GrayscaleAlpha, raw.chunks_exact(4).flat_map(|p| [p[0], p[3]]).collect()),
        (false, true) => (png::ColorType::Rgb, raw.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()),
        (false, false) => (png::ColorType::Rgba, raw.clone()),
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(compression(effort));
    finish(encoder, &data)?;
    Ok(out)
}

/// Packs one index per pixel into rows of `bits`-wide indices, as PNG stores them.
fn pack(width: u32, indices: &[u8], bits: u8) -> Vec<u8> {
    if bits == 8 { return indices.to_vec() }
    let mut out = Vec::with_capacity(indices.len() / 8 * usize::from(bits) + 8);
    for row in indices.chunks_exact(width as usize) {
        let mut byte = 0u8;
        let mut filled = 0u8;
        for index in row {
            byte = (byte << bits) | index;
            filled += bits;
            if filled == 8 {
                out.push(byte);
                (byte, filled) = (0, 0);
            }
        }
        if filled > 0 { out.push(byte << (8 - filled)); }
    }
    out
}

fn write_indexed(width: u32, height: u32, colors: &[[u8; 4]], indices: Vec<u8>, effort: Effort) -> Result<Vec<u8>, String> {
    let (bits, depth) = match colors.len() {
        0..=2 => (1, png::BitDepth::One),
        3..=4 => (2, png::BitDepth::Two),
        5..=16 => (4, png::BitDepth::Four),
        _ => (8, png::BitDepth::Eight),
    };
    let rgb: Vec<u8> = colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
    let alpha: Vec<u8> = colors.iter().map(|c| c[3]).collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(depth);
    encoder.set_palette(rgb);
    if alpha.iter().any(|a| *a != 255) { encoder.set_trns(alpha); }
    encoder.set_compression(compression(effort));
    // Indices have no gradient for a filter to find.
    encoder.set_filter(png::Filter::NoFilter);
    finish(encoder, &pack(width, &indices, bits))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            let v = (x.wrapping_mul(7919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761);
            image::Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255])
        })
    }

    fn decoded(bytes: &[u8]) -> RgbaImage {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap().into_rgba8()
    }

    #[test]
    fn lossless_keeps_every_pixel_and_fewer_colours_is_smaller() {
        let source = noise(128, 128);
        let (full, shown) = encode(&source, ImageEdit::default()).unwrap();
        assert_eq!(decoded(&full), source);
        assert_eq!(shown, source);
        let (small, shown) = encode(&source, ImageEdit { colors: Some(16), dither: false, effort: Effort::Best, ..ImageEdit::default() }).unwrap();
        assert!(small.len() < full.len() / 2, "{} against {}", small.len(), full.len());
        assert_eq!(decoded(&small), shown, "the preview is the decoded file");
    }

    #[test]
    fn an_image_with_few_colours_keeps_them_exactly_even_with_transparency() {
        let source = RgbaImage::from_fn(9, 9, |x, _| if x % 2 == 0 { image::Rgba([200, 10, 10, 255]) } else { image::Rgba([0, 0, 0, 0]) });
        let (bytes, shown) = encode(&source, ImageEdit { colors: Some(4), ..ImageEdit::default() }).unwrap();
        assert_eq!(shown, source);
        assert_eq!(decoded(&bytes), source, "packed to a narrow depth and read back unchanged");
    }

    #[test]
    fn data_maps_drop_channels_they_do_not_use() {
        let gray = RgbaImage::from_fn(64, 64, |x, y| { let v = (x * y) as u8; image::Rgba([v, v, v, 255]) });
        let (bytes, _) = encode(&gray, ImageEdit::default()).unwrap();
        let info = png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().unwrap().info().clone();
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        assert_eq!(decoded(&bytes), gray);
    }

    #[test]
    fn the_palette_never_exceeds_the_limit() {
        let source = noise(256, 256);
        for limit in [2u16, 16, 256] {
            let (bytes, shown) = encode(&source, ImageEdit { colors: Some(limit), dither: true, effort: Effort::Fast, ..ImageEdit::default() }).unwrap();
            let distinct: std::collections::HashSet<[u8; 4]> = shown.pixels().map(|p| p.0).collect();
            assert!(distinct.len() <= usize::from(limit), "{limit}: {}", distinct.len());
            assert_eq!(decoded(&bytes), shown);
        }
    }
}
