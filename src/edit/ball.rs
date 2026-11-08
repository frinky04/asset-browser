//! Material balls: a small sphere shaded in software with the same model as the preview stage, so
//! each row in the material list shows what that material actually looks like.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use glam::Vec3;

use super::mesh::SurfaceMaterial;
use crate::model::Image;
use crate::raster::LIGHT;

pub const BALL_PX: u32 = 160;
/// Textures are shrunk to about this before sampling: a ball this small would only alias them.
const SMALL_SIDE: u32 = 128;
/// How many times the texture wraps around the ball, so tiling materials read as tiling.
const WRAP: f32 = 2.0;

/// Shrunken copies of textures, kept for the session so a slider never redoes them. Each entry
/// holds its source, so the address it's keyed by stays taken.
#[derive(Default)]
pub struct Shrunk(Mutex<HashMap<usize, (Arc<Image>, Arc<Image>)>>);

impl Shrunk {
    pub fn get(&self, image: &Arc<Image>) -> Arc<Image> {
        let key = Arc::as_ptr(image) as usize;
        if let Ok(known) = self.0.lock() && let Some((_, small)) = known.get(&key) { return small.clone() }
        let small = shrink(image);
        if let Ok(mut known) = self.0.lock() { known.insert(key, (image.clone(), small.clone())); }
        small
    }
}

fn shrink(image: &Arc<Image>) -> Arc<Image> {
    if image.width.max(image.height) <= SMALL_SIDE { return image.clone() }
    let Some(buffer) = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone()) else { return image.clone() };
    let scale = SMALL_SIDE as f32 / image.width.max(image.height) as f32;
    let (w, h) = (((image.width as f32 * scale) as u32).max(1), ((image.height as f32 * scale) as u32).max(1));
    let out = image::imageops::thumbnail(&buffer, w, h);
    Arc::new(Image { width: w, height: h, rgba: out.into_raw() })
}

/// Bilinear, wrapping in both directions. Channels come back 0..1.
fn sample(image: &Image, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (image.width as usize, image.height as usize);
    let (x, y) = (u.rem_euclid(1.0) * w as f32 - 0.5, v.rem_euclid(1.0) * h as f32 - 0.5);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let at = |px: f32, py: f32| {
        let (ix, iy) = ((px as isize).rem_euclid(w as isize) as usize, (py as isize).rem_euclid(h as isize) as usize);
        &image.rgba[(iy * w + ix) * 4..(iy * w + ix) * 4 + 4]
    };
    let (a, b, c, d) = (at(x.floor(), y.floor()), at(x.floor() + 1.0, y.floor()), at(x.floor(), y.floor() + 1.0), at(x.floor() + 1.0, y.floor() + 1.0));
    std::array::from_fn(|i| {
        let top = f32::from(a[i]) * (1.0 - fx) + f32::from(b[i]) * fx;
        let bottom = f32::from(c[i]) * (1.0 - fx) + f32::from(d[i]) * fx;
        (top * (1.0 - fy) + bottom * fy) / 255.0
    })
}

fn mix(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

/// The textures a ball is drawn with, already shrunk.
#[derive(Default)]
pub struct Maps {
    pub base: Option<Arc<Image>>,
    pub surface: Option<Arc<Image>>,
    pub normal: Option<Arc<Image>>,
    pub emissive: Option<Arc<Image>>,
}

/// A `size`-square straight-alpha RGBA picture of the material on a sphere.
pub fn draw(m: &SurfaceMaterial, maps: &Maps, size: u32) -> Vec<u8> {
    let side = size as usize;
    let mut out = vec![0u8; side * side * 4];
    let radius = (size as f32 - 2.0) * 0.5;
    let centre = size as f32 * 0.5;
    let light = LIGHT.normalize();
    let eye = Vec3::Z;
    for y in 0..side {
        for x in 0..side {
            let (px, py) = ((x as f32 + 0.5 - centre) / radius, (centre - y as f32 - 0.5) / radius);
            let distance = (px * px + py * py).sqrt();
            let cover = ((1.0 - distance) * radius + 0.5).clamp(0.0, 1.0);
            if cover <= 0.0 { continue }
            let z = (1.0 - distance.min(1.0).powi(2)).max(0.0).sqrt();
            let mut n = Vec3::new(px.min(1.0), py.min(1.0), z).normalize_or_zero();
            let (u, v) = ((n.x.atan2(n.z) / (2.0 * PI) + 0.5) * WRAP, n.y.clamp(-1.0, 1.0).acos() / PI);

            let base = maps.base.as_ref().map_or([1.0; 4], |t| sample(t, u, v));
            if base[3] * m.color[3] < 0.5 { continue }
            let albedo = Vec3::new(base[0] * m.color[0], base[1] * m.color[1], base[2] * m.color[2]);
            if let Some(map) = &maps.normal {
                let texel = sample(map, u, v);
                let tangent = Vec3::Y.cross(n).normalize_or_zero();
                let bitangent = n.cross(tangent);
                let bump = Vec3::new((texel[0] * 2.0 - 1.0) * m.normal_scale, (texel[1] * 2.0 - 1.0) * m.normal_scale, texel[2] * 2.0 - 1.0);
                n = (tangent * bump.x + bitangent * bump.y + n * bump.z).normalize_or_zero();
            }
            let surface = maps.surface.as_ref().map_or([1.0; 4], |t| sample(t, u, v));
            let rough = (m.roughness * surface[1]).clamp(0.045, 1.0);
            let metal = (m.metallic * surface[2]).clamp(0.0, 1.0);

            // The same terms as `MODEL_WGSL`.
            let lit = 0.64 + 0.74 * n.dot(light).abs();
            let half = (light + eye).normalize();
            let a2 = (rough * rough).powi(2);
            let d = n.dot(half).max(0.0);
            let spread = d * d * (a2 - 1.0) + 1.0;
            let highlight = a2 / (PI * spread * spread).max(1e-4);
            let f0 = mix(Vec3::splat(0.04), albedo, metal);
            let toward = (-eye) - n * 2.0 * (-eye).dot(n);
            let sky = mix(Vec3::new(0.22, 0.23, 0.26), Vec3::new(0.98, 0.99, 1.0), toward.y * 0.5 + 0.5);
            let reflection = mix(sky, Vec3::splat(0.8), rough) * mix(Vec3::splat(1.0 - rough * 0.7), Vec3::ONE, metal);
            let mut shaded = albedo * lit * (1.0 - metal) + f0 * (reflection + Vec3::splat(highlight * n.dot(light).abs() * 0.25));
            let glow = maps.emissive.as_ref().map_or([1.0; 4], |t| sample(t, u, v));
            shaded += Vec3::new(m.emissive[0] * glow[0], m.emissive[1] * glow[1], m.emissive[2] * glow[2]);

            let at = (y * side + x) * 4;
            out[at] = (shaded.x.clamp(0.0, 1.0) * 255.0) as u8;
            out[at + 1] = (shaded.y.clamp(0.0, 1.0) * 255.0) as u8;
            out[at + 2] = (shaded.z.clamp(0.0, 1.0) * 255.0) as u8;
            out[at + 3] = (cover * 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(roughness: f32, metallic: f32) -> SurfaceMaterial {
        SurfaceMaterial { color: [0.6, 0.3, 0.2, 1.0], roughness, metallic, emissive: [0.0; 3], image: None, surface_image: None, normal_image: None, emissive_image: None, normal_scale: 1.0, doc: Some(0) }
    }

    fn pixel(px: &[u8], x: usize, y: usize, size: usize) -> [u8; 4] {
        let at = (y * size + x) * 4;
        [px[at], px[at + 1], px[at + 2], px[at + 3]]
    }

    fn brightest(px: &[u8]) -> u32 {
        px.chunks_exact(4).filter(|p| p[3] == 255).map(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])).max().unwrap()
    }

    #[test]
    fn the_ball_is_round_and_its_corners_are_clear() {
        let px = draw(&material(0.5, 0.0), &Maps::default(), 64);
        assert_eq!(pixel(&px, 0, 0, 64)[3], 0);
        assert_eq!(pixel(&px, 32, 32, 64)[3], 255);
    }

    #[test]
    fn a_glossy_ball_has_a_tighter_brighter_highlight_than_a_matte_one() {
        // Rough enough that the highlight is wider than a pixel at this size.
        let (glossy, matte) = (draw(&material(0.3, 0.0), &Maps::default(), 64), draw(&material(1.0, 0.0), &Maps::default(), 64));
        assert!(brightest(&glossy) > brightest(&matte) + 30, "{} against {}", brightest(&glossy), brightest(&matte));
    }

    #[test]
    fn its_texture_and_emission_reach_the_picture() {
        let blue = Arc::new(Image { width: 2, height: 2, rgba: [20, 40, 230, 255].repeat(4) });
        let plain = pixel(&draw(&material(1.0, 0.0), &Maps::default(), 64), 32, 32, 64);
        let textured = pixel(&draw(&material(1.0, 0.0), &Maps { base: Some(blue), ..Maps::default() }, 64), 32, 32, 64);
        assert!(textured[2] > textured[0], "{textured:?}");
        assert!(plain[0] > plain[2], "{plain:?}");
        let glowing = SurfaceMaterial { emissive: [0.0, 0.8, 0.0], ..material(1.0, 0.0) };
        assert!(pixel(&draw(&glowing, &Maps::default(), 64), 32, 32, 64)[1] > plain[1] + 100);
    }

    #[test]
    fn a_rough_metal_stays_about_as_bright_as_its_colour() {
        let px = draw(&material(1.0, 1.0), &Maps::default(), 64);
        let centre = pixel(&px, 32, 32, 64);
        assert!(f32::from(centre[0]) > 0.6 * 255.0 * 0.7, "{centre:?}");
    }

    #[test]
    fn a_normal_map_changes_the_shading() {
        let tilted = Arc::new(Image { width: 2, height: 2, rgba: [230, 128, 180, 255].repeat(4) });
        let flat = draw(&material(0.4, 0.0), &Maps::default(), 64);
        let bumped = draw(&material(0.4, 0.0), &Maps { normal: Some(tilted), ..Maps::default() }, 64);
        assert_ne!(flat, bumped);
    }

    #[test]
    fn big_textures_are_shrunk_once_and_small_ones_are_left_alone() {
        let shrunk = Shrunk::default();
        let big = Arc::new(Image { width: 512, height: 256, rgba: vec![9; 512 * 256 * 4] });
        let small = shrunk.get(&big);
        assert_eq!((small.width, small.height), (128, 64));
        assert!(Arc::ptr_eq(&small, &shrunk.get(&big)));
        let tiny = Arc::new(Image { width: 8, height: 8, rgba: vec![9; 8 * 8 * 4] });
        assert!(Arc::ptr_eq(&tiny, &shrunk.get(&tiny)));
    }
}
