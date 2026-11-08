//! A small software rasterizer for model thumbnails: no GPU work competes with the window, and
//! it runs on any worker thread.

use glam::{Mat4, Vec3, Vec4};

use crate::model::Model;

/// Device pixels the thumbnail is supersampled by on each axis.
const SUPER: u32 = 2;

/// The direction light comes from, shared with the interactive preview.
pub const LIGHT: Vec3 = Vec3::new(0.45, 0.8, 0.4);

/// A three-quarter view of `model` as `size`² straight-alpha RGBA.
pub fn render_thumb(model: &Model, size: u32) -> Vec<u8> {
    let full = size * SUPER;
    let (center, radius) = model.bounds();
    let view = Mat4::from_rotation_x(0.42) * Mat4::from_rotation_y(-0.65) * Mat4::from_translation(-center);
    let distance = radius * 3.0;
    let camera = Mat4::from_translation(Vec3::new(0.0, 0.0, -distance));
    let proj = Mat4::perspective_rh(38f32.to_radians(), 1.0, radius * 0.1, distance + radius * 3.0);
    let clip = proj * camera * view;
    let light = view.transform_vector3(LIGHT.normalize());

    let mut color = vec![[0f32; 4]; (full * full) as usize];
    let mut depth = vec![f32::MAX; (full * full) as usize];

    struct Corner { x: f32, y: f32, inv_w: f32, depth: f32 }

    for part in &model.parts {
        let material = model.materials[part.material as usize];
        let image = material.image.and_then(|i| model.images.get(i as usize));
        let tris = &model.indices[part.start as usize..(part.start + part.count) as usize];
        for tri in tris.chunks_exact(3) {
            let vs = [model.vertices[tri[0] as usize], model.vertices[tri[1] as usize], model.vertices[tri[2] as usize]];
            let mut c: [Option<Corner>; 3] = [None, None, None];
            let mut behind = false;
            for (i, v) in vs.iter().enumerate() {
                let p: Vec4 = clip * Vec3::from(v.pos).extend(1.0);
                if p.w <= radius * 0.01 { behind = true; break }
                c[i] = Some(Corner {
                    x: (p.x / p.w * 0.5 + 0.5) * full as f32,
                    y: (0.5 - p.y / p.w * 0.5) * full as f32,
                    inv_w: 1.0 / p.w,
                    depth: p.z / p.w,
                });
            }
            let ([Some(a), Some(b), Some(c)], false) = (c, behind) else { continue };
            let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
            if area.abs() < 1e-4 { continue }
            let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as u32;
            let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as u32;
            let max_x = (a.x.max(b.x).max(c.x).ceil() as i64).clamp(0, i64::from(full) - 1) as u32;
            let max_y = (a.y.max(b.y).max(c.y).ceil() as i64).clamp(0, i64::from(full) - 1) as u32;
            for y in min_y..=max_y {
                let py = y as f32 + 0.5;
                for x in min_x..=max_x {
                    let px = x as f32 + 0.5;
                    let w0 = ((b.x - px) * (c.y - py) - (b.y - py) * (c.x - px)) / area;
                    let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 { continue }
                    let z = w0 * a.depth + w1 * b.depth + w2 * c.depth;
                    let slot = (y * full + x) as usize;
                    if z >= depth[slot] || z < 0.0 { continue }
                    let inv_w = w0 * a.inv_w + w1 * b.inv_w + w2 * c.inv_w;
                    let persp = |p: f32, q: f32, r: f32| (w0 * a.inv_w * p + w1 * b.inv_w * q + w2 * c.inv_w * r) / inv_w;
                    let mut albedo = material.color;
                    if let Some(img) = image {
                        let u = persp(vs[0].uv[0], vs[1].uv[0], vs[2].uv[0]).rem_euclid(1.0);
                        let v = persp(vs[0].uv[1], vs[1].uv[1], vs[2].uv[1]).rem_euclid(1.0);
                        let tx = ((u * img.width as f32) as u32).min(img.width - 1);
                        let ty = ((v * img.height as f32) as u32).min(img.height - 1);
                        let t = (ty * img.width + tx) as usize * 4;
                        for k in 0..4 { albedo[k] *= f32::from(img.rgba[t + k]) / 255.0; }
                    }
                    if albedo[3] < 0.5 { continue }
                    let n = Vec3::new(
                        persp(vs[0].normal[0], vs[1].normal[0], vs[2].normal[0]),
                        persp(vs[0].normal[1], vs[1].normal[1], vs[2].normal[1]),
                        persp(vs[0].normal[2], vs[1].normal[2], vs[2].normal[2]),
                    );
                    let n = view.transform_vector3(n).normalize_or_zero();
                    // Brighter fill than the preview: a thumbnail has to read at a glance.
                    let lit = 0.82 + 0.72 * n.dot(light).abs();
                    depth[slot] = z;
                    color[slot] = [albedo[0] * lit, albedo[1] * lit, albedo[2] * lit, 1.0];
                }
            }
        }
    }

    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0f32; 4];
            for sy in 0..SUPER {
                for sx in 0..SUPER {
                    let s = color[((y * SUPER + sy) * full + x * SUPER + sx) as usize];
                    for k in 0..4 { sum[k] += s[k]; }
                }
            }
            let at = ((y * size + x) * 4) as usize;
            let alpha = sum[3] / (SUPER * SUPER) as f32;
            if alpha > 0.0 {
                let scale = sum[3].max(1.0);
                for k in 0..3 { out[at + k] = ((sum[k] / scale).clamp(0.0, 1.0) * 255.0) as u8; }
                out[at + 3] = (alpha * 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Material, Part, Vertex};

    #[test]
    fn a_triangle_draws_opaque_pixels_and_leaves_the_rest_clear() {
        let v = |x: f32, y: f32| Vertex { pos: [x, y, 0.0], normal: [0.0, 0.0, 1.0], uv: [0.0; 2] };
        let model = Model {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            parts: vec![Part { start: 0, count: 3, material: 0 }],
            materials: vec![Material { color: [1.0, 0.0, 0.0, 1.0], image: None }],
            images: vec![],
            ..Default::default()
        };
        let px = render_thumb(&model, 32);
        let solid = px.chunks(4).filter(|p| p[3] == 255).count();
        assert!(solid > 40, "{solid}");
        assert_eq!(px[3], 0, "the corner stays clear");
        let red = px.chunks(4).find(|p| p[3] == 255).unwrap();
        assert!(red[0] > 100 && red[1] == 0);
    }
}
