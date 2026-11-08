//! The preview stage: textures and models drawn with wgpu into Rune's effect slot. Each frame
//! renders to an offscreen target (it needs depth and MSAA that Rune's pass doesn't have), and
//! the slot's hook copies that target into place.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use rune::{Compositor, Frame};
use wgpu::util::DeviceExt;

use crate::app::App;
use crate::edit::Surface;
use crate::edit::mesh::TextureFilter;
use crate::model::{self, Image, Model};
use crate::raster::LIGHT;

/// The slot's `El::effect` mark and the element key the stage's size is read from.
pub const STAGE_MARK: u32 = 1;
pub const STAGE_KEY: &str = "stage";
pub const FOV_DEGREES: f32 = 38.0;
/// The stage's corner radius in points, matching the wells around it.
const CORNER_RADIUS: f32 = 8.0;

#[derive(Clone)]
pub enum Scene {
    Empty,
    Texture(Arc<Image>),
    Model(Arc<Model>),
}

/// Where the preview is looking. Handlers write it; the compositor eases toward it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub yaw: f32,
    pub pitch: f32,
    /// Camera distance in model radii.
    pub distance: f32,
    /// Model pan, in radii.
    pub pan: [f32; 2],
    /// Texture scale relative to fitting the stage.
    pub zoom: f32,
    /// Texture pan, in points from centre.
    pub offset: [f32; 2],
    pub tile: bool,
    pub seams: bool,
    pub smooth: bool,
}

impl Default for View {
    fn default() -> Self {
        View { yaw: -0.65, pitch: 0.42, distance: 3.3, pan: [0.0; 2], zoom: 1.0, offset: [0.0; 2], tile: false, seams: false, smooth: false }
    }
}

impl View {
    fn eased(self, to: View, k: f32) -> View {
        let mix = |a: f32, b: f32| a + (b - a) * k;
        // Scale eases in log space, so zooming in and out feel the same.
        let ratio = |a: f32, b: f32| (a.ln() + (b.ln() - a.ln()) * k).exp();
        View {
            yaw: mix(self.yaw, to.yaw),
            pitch: mix(self.pitch, to.pitch),
            distance: ratio(self.distance, to.distance),
            pan: [mix(self.pan[0], to.pan[0]), mix(self.pan[1], to.pan[1])],
            zoom: ratio(self.zoom, to.zoom),
            offset: [mix(self.offset[0], to.offset[0]), mix(self.offset[1], to.offset[1])],
            ..to
        }
    }

    fn close_to(&self, to: &View) -> bool {
        (self.yaw - to.yaw).abs() < 1e-3 && (self.pitch - to.pitch).abs() < 1e-3
            && (self.distance / to.distance - 1.0).abs() < 1e-3 && (self.zoom / to.zoom - 1.0).abs() < 1e-3
            && (self.pan[0] - to.pan[0]).abs() < 1e-3 && (self.pan[1] - to.pan[1]).abs() < 1e-3
            && (self.offset[0] - to.offset[0]).abs() < 0.05 && (self.offset[1] - to.offset[1]).abs() < 0.05
    }
}

pub struct StageState {
    pub scene: Scene,
    /// Bumped when `scene` changes.
    pub revision: u64,
    pub view: View,
    /// Bumped when the camera should jump rather than ease.
    pub snap: u64,
    pub look: Look,
    /// How quickly the camera eases, relative to normal. 0 moves it at once.
    pub ease: f32,
    /// The materials of a model being edited; `None` draws the model as it loaded.
    pub surface: Option<Surface>,
    /// Bumped when `surface` changes.
    pub surface_revision: u64,
}

/// Colours the stage paints itself, sRGB-encoded, to match the UI's theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub backdrop: [f32; 3],
    /// The two squares of the checkerboard behind transparent textures.
    pub check: [f32; 2],
}

impl Look {
    pub const DARK: Look = Look { backdrop: [0.106, 0.106, 0.106], check: [0.16, 0.22] };
    pub const LIGHT: Look = Look { backdrop: [0.929, 0.929, 0.937], check: [0.80, 0.90] };
}

impl Default for StageState {
    fn default() -> Self {
        StageState { scene: Scene::Empty, revision: 0, view: View::default(), snap: 0, look: Look::DARK, ease: 1.0, surface: None, surface_revision: 0 }
    }
}

/// Points to scale at which the texture fills the stage, for a stage of `stage` points.
pub fn fit_scale(stage: [f32; 2], texture: (u32, u32)) -> f32 {
    (stage[0] / texture.0.max(1) as f32).min(stage[1] / texture.1.max(1) as f32)
}

/// The zoom that shows one texel per device pixel.
pub fn actual_size_zoom(stage: [f32; 2], texture: (u32, u32), scale: f32) -> f32 {
    1.0 / (fit_scale(stage, texture) * scale)
}

/// Pan that keeps the point under `at` still while the zoom changes by `ratio`.
pub fn zoom_about(offset: [f32; 2], at: [f32; 2], ratio: f32) -> [f32; 2] {
    [at[0] - (at[0] - offset[0]) * ratio, at[1] - (at[1] - offset[1]) * ratio]
}

const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;

const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var nearest: sampler;
struct Shape { size: vec2f, radius: f32, pad: f32 }
@group(0) @binding(2) var<uniform> shape: Shape;
struct V { @builtin(position) p: vec4f, @location(0) uv: vec2f }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return V(vec4f(uv * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0), uv);
}
@fragment fn fs(v: V) -> @location(0) vec4f {
    let half = shape.size * 0.5;
    let q = abs(v.uv * shape.size - half) - half + vec2f(shape.radius);
    let d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - shape.radius;
    let cover = clamp(0.5 - d, 0.0, 1.0);
    let c = textureSample(image, nearest, v.uv);
    return vec4f(c.rgb * cover, cover);
}
"#;

const TEXTURE_WGSL: &str = r#"
struct U { view: vec2f, size: vec2f, offset: vec2f, zoom: f32, tile: f32, seams: f32, check_a: f32, check_b: f32, back: vec3f }
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
struct V { @builtin(position) p: vec4f, @location(0) px: vec2f }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return V(vec4f(uv * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0), uv * u.view);
}
@fragment fn fs(v: V) -> @location(0) vec4f {
    let uv = (v.px - u.view * 0.5 - u.offset) / (u.zoom * u.size) + vec2f(0.5);
    let texel = textureSample(image, smp, uv);
    let cell = floor(v.px / 9.0);
    let odd = (cell.x + cell.y) % 2.0;
    let check = mix(vec3f(u.check_a), vec3f(u.check_b), abs(odd));
    var out = mix(check, texel.rgb, texel.a);
    let inside = all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0));
    if (u.tile < 0.5 && !inside) { out = u.back; }
    if (u.tile > 0.5 && u.seams > 0.5) {
        let edge = abs(fract(uv + vec2f(0.5)) - vec2f(0.5)) * u.zoom * u.size;
        if (min(edge.x, edge.y) < 0.75) { out = vec3f(0.95, 0.70, 0.26); }
    }
    return vec4f(out, 1.0);
}
"#;

// `pbr` is roughness, metallic, and whether to shade the surface at all: browsing keeps the flat
// studio look, and only a model being edited gets highlights and reflections.
const MODEL_WGSL: &str = r#"
struct G { clip: mat4x4f, light: vec4f, eye: vec4f }
@group(0) @binding(0) var<uniform> g: G;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;
struct M { tint: vec4f, pbr: vec4f, emissive: vec4f }
@group(1) @binding(2) var<uniform> m: M;
@group(1) @binding(3) var surface_map: texture_2d<f32>;
@group(1) @binding(4) var normal_map: texture_2d<f32>;
@group(1) @binding(5) var emissive_map: texture_2d<f32>;
// Each map has its own sampler, because each can have its own filter.
@group(1) @binding(6) var surface_smp: sampler;
@group(1) @binding(7) var normal_smp: sampler;
@group(1) @binding(8) var emissive_smp: sampler;
// Bends `n` by a tangent-space sample, finding the tangent frame from screen derivatives so the
// mesh needs no tangents of its own. A glTF normal map's +Y points against increasing v.
fn bent(n: vec3f, world: vec3f, uv: vec2f, texel: vec3f, scale: f32) -> vec3f {
    let dp1 = dpdx(world);
    let dp2 = dpdy(world);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let across = cross(dp2, n);
    let along = cross(n, dp1);
    let t = across * duv1.x + along * duv2.x;
    let b = across * duv1.y + along * duv2.y;
    let longest = max(dot(t, t), dot(b, b));
    if (longest < 1e-14) { return n; }
    let unit = 1.0 / sqrt(longest);
    let bump = (texel * 2.0 - vec3f(1.0)) * vec3f(scale, scale, 1.0);
    return normalize(t * unit * bump.x - b * unit * bump.y + n * bump.z);
}
struct V { @builtin(position) p: vec4f, @location(0) n: vec3f, @location(1) uv: vec2f, @location(2) world: vec3f }
@vertex fn vs(@location(0) pos: vec3f, @location(1) n: vec3f, @location(2) uv: vec2f) -> V {
    return V(g.clip * vec4f(pos, 1.0), n, uv, pos);
}
@fragment fn fs(v: V) -> @location(0) vec4f {
    let albedo = textureSample(image, smp, v.uv) * m.tint;
    if (albedo.a < 0.5) { discard; }
    var n = normalize(v.n);
    let flat_lit = 0.64 + 0.74 * abs(dot(n, g.light.xyz));
    let map = textureSample(surface_map, surface_smp, v.uv);
    let normal_texel = textureSample(normal_map, normal_smp, v.uv).xyz;
    let glow = textureSample(emissive_map, emissive_smp, v.uv).rgb;
    if (m.pbr.z < 0.5) { return vec4f(albedo.rgb * flat_lit, 1.0); }
    let rough = clamp(m.pbr.x * map.g, 0.045, 1.0);
    let metal = clamp(m.pbr.y * map.b, 0.0, 1.0);
    let eye = normalize(g.eye.xyz - v.world);
    if (dot(n, eye) < 0.0) { n = -n; }
    n = bent(n, v.world, v.uv, normal_texel, m.emissive.w);
    let lit = 0.64 + 0.74 * abs(dot(n, g.light.xyz));
    let half = normalize(g.light.xyz + eye);
    let a = rough * rough;
    let a2 = a * a;
    let d = max(dot(n, half), 0.0);
    let spread = d * d * (a2 - 1.0) + 1.0;
    let highlight = a2 / max(3.14159 * spread * spread, 1e-4);
    let f0 = mix(vec3f(0.04), albedo.rgb, metal);
    let toward = reflect(-eye, n);
    let sky = mix(vec3f(0.22, 0.23, 0.26), vec3f(0.98, 0.99, 1.0), toward.y * 0.5 + 0.5);
    // A rough metal still reflects the whole room, just blurred, so it stays as bright as its colour.
    let reflection = mix(sky, vec3f(0.8), rough) * mix(1.0 - rough * 0.7, 1.0, metal);
    let shaded = albedo.rgb * lit * (1.0 - metal) + f0 * (highlight * abs(dot(n, g.light.xyz)) * 0.25 + reflection);
    let finished = shaded + m.emissive.rgb * glow;
    // Hovering a material in the list leaves the others like this.
    return vec4f(mix(finished * 0.28, finished, m.pbr.w), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureUniform {
    view: [f32; 2],
    size: [f32; 2],
    offset: [f32; 2],
    zoom: f32,
    tile: f32,
    seams: f32,
    check: [f32; 2],
    pad: f32,
    back: [f32; 3],
    end: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ModelUniform {
    clip: [[f32; 4]; 4],
    light: [f32; 4],
    eye: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PartUniform {
    tint: [f32; 4],
    pbr: [f32; 4],
    emissive: [f32; 4],
}

/// Box-filtered mip chain of straight-alpha RGBA, level 0 first and borrowed from the image.
fn mips(image: &Image) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = vec![(image.width, image.height, image.rgba.clone())];
    while let Some((w, h, px)) = out.last() {
        let (w, h) = (*w, *h);
        if w == 1 && h == 1 { break }
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0u32; 4];
                for sy in 0..=u32::from(h > 1) {
                    for sx in 0..=u32::from(w > 1) {
                        let at = (((y * (h / nh) + sy).min(h - 1) * w + (x * (w / nw) + sx).min(w - 1)) * 4) as usize;
                        for c in 0..4 { sum[c] += u32::from(px[at + c]); }
                    }
                }
                let n = (1 + u32::from(h > 1)) * (1 + u32::from(w > 1));
                let at = ((y * nw + x) * 4) as usize;
                for c in 0..4 { next[at + c] = (sum[c] / n) as u8; }
            }
        }
        out.push((nw, nh, next));
    }
    out
}

fn upload_image(device: &wgpu::Device, queue: &wgpu::Queue, image: &Image) -> wgpu::TextureView {
    let chain = mips(image);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("preview image"),
        size: wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 },
        mip_level_count: chain.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (w, h, px)) in chain.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            px,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(*h) },
            wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
        );
    }
    texture.create_view(&Default::default())
}

struct Pipes {
    key: rune::compose::DeviceKey,
    blit_layout: wgpu::BindGroupLayout,
    blit_pipeline_layout: wgpu::PipelineLayout,
    blit_module: wgpu::ShaderModule,
    blit_shape: wgpu::Buffer,
    blits: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    texture_layout: wgpu::BindGroupLayout,
    texture_pipe: wgpu::RenderPipeline,
    texture_uniform: wgpu::Buffer,
    part_layout: wgpu::BindGroupLayout,
    model_pipe: wgpu::RenderPipeline,
    model_uniform: wgpu::Buffer,
    global_group: wgpu::BindGroup,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    repeat_nearest: wgpu::Sampler,
    repeat_linear: wgpu::Sampler,
    /// Nearest at every step, as a model's Pixels filter asks for.
    repeat_point: wgpu::Sampler,
}

fn sampler(device: &wgpu::Device, mag: wgpu::FilterMode, address: wgpu::AddressMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: address,
        address_mode_v: address,
        mag_filter: mag,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }
}

impl Pipes {
    fn new(device: &wgpu::Device, key: rune::compose::DeviceKey) -> Pipes {
        let module = |label: &str, source: &str| device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
        let bgl = |entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries });
        let frag_vert = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;

        let blit_layout = bgl(&[tex_entry(0), sampler_entry(1), uniform_entry(2, wgpu::ShaderStages::FRAGMENT)]);
        let blit_shape = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&blit_layout)], immediate_size: 0 });
        let blit_module = module("blit", BLIT_WGSL);

        let texture_layout = bgl(&[uniform_entry(0, frag_vert), tex_entry(1), sampler_entry(2)]);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&texture_layout)], immediate_size: 0 });
        let m = module("texture", TEXTURE_WGSL);
        let texture_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("texture view"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &m, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &m, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(COLOR.into())] }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = |size: usize| device.create_buffer(&wgpu::BufferDescriptor { label: None, size: size as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let texture_uniform = uniform(std::mem::size_of::<TextureUniform>());

        let global_layout = bgl(&[uniform_entry(0, frag_vert)]);
        let part_layout = bgl(&[tex_entry(0), sampler_entry(1), uniform_entry(2, wgpu::ShaderStages::FRAGMENT), tex_entry(3), tex_entry(4), tex_entry(5), sampler_entry(6), sampler_entry(7), sampler_entry(8)]);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&global_layout), Some(&part_layout)], immediate_size: 0 });
        let m = module("model", MODEL_WGSL);
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
        let model_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("model view"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &m,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<model::Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            fragment: Some(wgpu::FragmentState { module: &m, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(COLOR.into())] }),
            multiview_mask: None,
            cache: None,
        });
        let model_uniform = uniform(std::mem::size_of::<ModelUniform>());
        let global_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &global_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: model_uniform.as_entire_binding() }],
        });
        Pipes {
            key,
            blit_layout,
            blit_pipeline_layout,
            blit_module,
            blit_shape,
            blits: HashMap::new(),
            texture_layout,
            texture_pipe,
            texture_uniform,
            part_layout,
            model_pipe,
            model_uniform,
            global_group,
            nearest: sampler(device, wgpu::FilterMode::Nearest, wgpu::AddressMode::ClampToEdge),
            linear: sampler(device, wgpu::FilterMode::Linear, wgpu::AddressMode::ClampToEdge),
            repeat_nearest: sampler(device, wgpu::FilterMode::Nearest, wgpu::AddressMode::Repeat),
            repeat_linear: sampler(device, wgpu::FilterMode::Linear, wgpu::AddressMode::Repeat),
            repeat_point: device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            }),
        }
    }

    fn blit(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        let (layout, module) = (&self.blit_pipeline_layout, &self.blit_module);
        self.blits.entry(format).or_insert_with(|| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stage blit"),
            layout: Some(layout),
            vertex: wgpu::VertexState { module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        }))
    }
}

struct Target {
    size: (u32, u32),
    view: wgpu::TextureView,
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    group: wgpu::BindGroup,
}

impl Target {
    fn new(device: &wgpu::Device, pipes: &Pipes, size: (u32, u32)) -> Target {
        let make = |format, samples, usage| device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stage target"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        }).create_view(&Default::default());
        let view = make(COLOR, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipes.blit_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&pipes.nearest) },
                wgpu::BindGroupEntry { binding: 2, resource: pipes.blit_shape.as_entire_binding() },
            ],
        });
        Target {
            size,
            msaa: make(COLOR, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT),
            depth: make(DEPTH, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT),
            view,
            group,
        }
    }
}

enum GpuScene {
    Texture { size: (u32, u32), groups: [[wgpu::BindGroup; 2]; 2] },
    Model { vertices: wgpu::Buffer, indices: wgpu::Buffer, parts: Vec<GpuPart>, images: ModelImages, marker: OriginMarker, center: Vec3, radius: f32 },
}

struct GpuPart {
    start: u32,
    count: u32,
    material: u32,
    uniform: wgpu::Buffer,
    group: wgpu::BindGroup,
}

/// The textures a model is drawn with, uploaded once each however many times they're rebound.
struct ModelImages {
    white: wgpu::TextureView,
    /// A normal map that doesn't bend anything.
    flat: wgpu::TextureView,
    /// Keyed by the image's address, which the `Arc` or `Model` holding it keeps stable.
    views: HashMap<usize, (wgpu::TextureView, Option<Arc<Image>>)>,
}

impl ModelImages {
    fn view(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, image: &Image, keep: Option<&Arc<Image>>) -> wgpu::TextureView {
        let (view, _) = self.views.entry(std::ptr::from_ref(image) as usize).or_insert_with(|| (upload_image(device, queue, image), keep.cloned()));
        view.clone()
    }
}

/// Three arrows at a model's origin, drawn over it so an origin inside the mesh still shows.
struct OriginMarker {
    vertices: wgpu::Buffer,
    groups: [wgpu::BindGroup; 3],
    visible: bool,
}

/// Each axis is a four-sided spike of this many corners.
const MARKER_AXIS_VERTICES: usize = 12;
const MARKER_TINTS: [[f32; 4]; 3] = [[0.92, 0.26, 0.26, 1.0], [0.36, 0.78, 0.30, 1.0], [0.28, 0.50, 0.98, 1.0]];

fn marker_vertices(at: Vec3, radius: f32) -> Vec<model::Vertex> {
    let (length, width) = (radius * 0.5, radius * 0.025);
    let normal = LIGHT.normalize().to_array();
    let vertex = |pos: Vec3| model::Vertex { pos: pos.to_array(), normal, uv: [0.0; 2] };
    let mut out = Vec::with_capacity(MARKER_AXIS_VERTICES * 3);
    for (axis, a, b) in [(Vec3::X, Vec3::Y, Vec3::Z), (Vec3::Y, Vec3::Z, Vec3::X), (Vec3::Z, Vec3::X, Vec3::Y)] {
        let tip = at + axis * length;
        let corners = [at + a * width, at + b * width, at - a * width, at - b * width];
        for side in 0..4 { out.extend([vertex(tip), vertex(corners[side]), vertex(corners[(side + 1) % 4])]); }
    }
    out
}

fn update_marker(queue: &wgpu::Queue, marker: &mut OriginMarker, surface: Option<&Surface>, radius: f32) {
    let origin = surface.and_then(|s| s.origin);
    marker.visible = origin.is_some();
    if let Some(at) = origin { queue.write_buffer(&marker.vertices, 0, bytemuck::cast_slice(&marker_vertices(Vec3::from(at), radius))); }
}

/// The sampler for a texture filter. A file that doesn't say draws as models do while browsing.
fn model_sampler(pipes: &Pipes, filter: Option<TextureFilter>) -> &wgpu::Sampler {
    match filter {
        None => &pipes.repeat_nearest,
        Some(TextureFilter::Smooth) => &pipes.repeat_linear,
        Some(TextureFilter::Pixels) => &pipes.repeat_point,
    }
}

/// Binds each part to its current colour, surface and textures. Runs when a model appears and
/// when its materials change, so a slider costs a uniform write and not a re-upload.
fn bind_parts(device: &wgpu::Device, queue: &wgpu::Queue, pipes: &Pipes, parts: &mut [GpuPart], images: &mut ModelImages, model: &Model, surface: Option<&Surface>) {
    let mut used = std::collections::HashSet::new();
    for part in parts {
        let shaded = surface.and_then(|s| s.materials.get(part.material as usize).map(|m| (s, *m)));
        let (uniform, base, map, normal, glow) = if let Some((surface, m)) = shaded {
            let view_of = |index: Option<u32>, images: &mut ModelImages, used: &mut std::collections::HashSet<usize>| {
                let image = index.and_then(|i| surface.images.get(i as usize))?;
                used.insert(Arc::as_ptr(image) as usize);
                Some(images.view(device, queue, image, Some(image)))
            };
            let base = view_of(m.image, images, &mut used);
            let map = view_of(m.surface_image, images, &mut used);
            let normal = view_of(m.normal_image, images, &mut used);
            let glow = view_of(m.emissive_image, images, &mut used);
            let picked = surface.focus.is_none_or(|focus| m.doc == Some(focus as u32));
            (PartUniform { tint: m.color, pbr: [m.roughness, m.metallic, 1.0, f32::from(u8::from(picked))], emissive: [m.emissive[0], m.emissive[1], m.emissive[2], m.normal_scale] }, base, map, normal, glow)
        } else {
            let material = model.materials[part.material as usize];
            let base = material.image.and_then(|i| model.images.get(i as usize)).map(|image| {
                used.insert(std::ptr::from_ref(image) as usize);
                images.view(device, queue, image, None)
            });
            (PartUniform { tint: material.color, pbr: [1.0, 0.0, 0.0, 1.0], emissive: [0.0; 4] }, base, None, None, None)
        };
        queue.write_buffer(&part.uniform, 0, bytemuck::bytes_of(&uniform));
        let white = images.white.clone();
        let flat = images.flat.clone();
        let filters = shaded.map_or([None; 4], |(_, m)| m.filters);
        part.group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipes.part_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(base.as_ref().unwrap_or(&white)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(model_sampler(pipes, filters[0])) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(model_sampler(pipes, filters[1])) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(model_sampler(pipes, filters[2])) },
                wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(model_sampler(pipes, filters[3])) },
                wgpu::BindGroupEntry { binding: 2, resource: part.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(map.as_ref().unwrap_or(&white)) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(normal.as_ref().unwrap_or(&flat)) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(glow.as_ref().unwrap_or(&white)) },
            ],
        });
    }
    images.views.retain(|address, _| used.contains(address));
}

fn prepare_scene(device: &wgpu::Device, queue: &wgpu::Queue, pipes: &Pipes, scene: &Scene, surface: Option<&Surface>) -> Option<GpuScene> {
    match scene {
        Scene::Empty => None,
        Scene::Texture(image) => {
            let view = upload_image(device, queue, image);
            let group = |s: &wgpu::Sampler| device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipes.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: pipes.texture_uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(s) },
                ],
            });
            Some(GpuScene::Texture {
                size: (image.width, image.height),
                groups: [[group(&pipes.nearest), group(&pipes.linear)], [group(&pipes.repeat_nearest), group(&pipes.repeat_linear)]],
            })
        }
        Scene::Model(m) => {
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&m.vertices), usage: wgpu::BufferUsages::VERTEX });
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX });
            let white = upload_image(device, queue, &Image { width: 1, height: 1, rgba: vec![255; 4] });
            let flat = upload_image(device, queue, &Image { width: 1, height: 1, rgba: vec![128, 128, 255, 255] });
            let mut images = ModelImages { white: white.clone(), flat: flat.clone(), views: HashMap::new() };
            let mut parts: Vec<GpuPart> = m.parts.iter().map(|p| {
                let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: std::mem::size_of::<PartUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &pipes.part_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&white) },
                        wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&flat) },
                        wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&white) },
                    ],
                });
                GpuPart { start: p.start, count: p.count, material: p.material, uniform, group }
            }).collect();
            bind_parts(device, queue, pipes, &mut parts, &mut images, m, surface);
            let (center, radius) = m.bounds();
            let marker_vertex_size = (MARKER_AXIS_VERTICES * 3 * std::mem::size_of::<model::Vertex>()) as u64;
            let groups = MARKER_TINTS.map(|tint| {
                let uniform = PartUniform { tint, pbr: [1.0, 0.0, 0.0, 1.0], emissive: [0.0; 4] };
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::bytes_of(&uniform), usage: wgpu::BufferUsages::UNIFORM });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &pipes.part_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&white) },
                        wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&flat) },
                        wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&white) },
                        wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                        wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&pipes.repeat_nearest) },
                    ],
                })
            });
            let mut marker = OriginMarker {
                vertices: device.create_buffer(&wgpu::BufferDescriptor { label: Some("origin marker"), size: marker_vertex_size, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
                groups,
                visible: false,
            };
            update_marker(queue, &mut marker, surface, radius);
            Some(GpuScene::Model { vertices, indices, parts, images, marker, center, radius })
        }
    }
}

#[derive(Default)]
pub struct StageCompositor {
    pipes: Option<Pipes>,
    target: Option<Target>,
    scene: Option<GpuScene>,
    seen_revision: u64,
    seen_surface: u64,
    seen_snap: u64,
    current: Option<View>,
    moving: bool,
}

impl Compositor<App> for StageCompositor {
    fn render(&mut self, f: &mut Frame<'_, App>) {
        let clear = f.rt.clear_color();
        let scale = f.size.0 as f32 / f.rt.viewport.0.max(1.0);
        f.rt.state.scale = scale;
        let rect = f.rt.presented(STAGE_KEY).map(|(r, _)| r);
        let device_key = f.device_key();
        if self.pipes.as_ref().is_none_or(|p| p.key != device_key) {
            self.pipes = Some(Pipes::new(f.device, device_key));
            self.target = None;
            self.scene = None;
            self.seen_revision = 0;
        }
        let Some(pipes) = self.pipes.as_mut() else { return };

        let state = &f.rt.state.stage;
        if state.revision != self.seen_revision {
            self.scene = prepare_scene(f.device, f.queue, pipes, &state.scene, state.surface.as_ref());
            self.seen_revision = state.revision;
            self.seen_surface = state.surface_revision;
        } else if state.surface_revision != self.seen_surface {
            self.seen_surface = state.surface_revision;
            if let (Some(GpuScene::Model { parts, images, marker, radius, .. }), Scene::Model(model)) = (&mut self.scene, &state.scene) {
                bind_parts(f.device, f.queue, pipes, parts, images, model, state.surface.as_ref());
                update_marker(f.queue, marker, state.surface.as_ref(), *radius);
            }
        }
        // Ease the camera; a headless run lands at once, so a snapshot is exact.
        let look = state.look;
        let target = state.view;
        let ease = state.ease;
        let jump = state.snap != self.seen_snap || ease <= 0.0 || rune::tooling::headless_run().is_some();
        self.seen_snap = state.snap;
        let view = match self.current {
            Some(current) if !jump => {
                let eased = current.eased(target, 1.0 - (-(f.dt.min(0.1) as f32) * 24.0 * ease).exp());
                if eased.close_to(&target) { target } else { eased }
            }
            _ => target,
        };
        self.moving = view != target;
        self.current = Some(view);

        let drawn = match (rect, &self.scene) {
            (Some(r), Some(scene)) if r[2] >= 1.0 && r[3] >= 1.0 => {
                let size = ((r[2] * scale).round().max(1.0) as u32, (r[3] * scale).round().max(1.0) as u32);
                if self.target.as_ref().is_none_or(|t| t.size != size) {
                    self.target = Some(Target::new(f.device, pipes, size));
                }
                f.queue.write_buffer(&pipes.blit_shape, 0, bytemuck::cast_slice(&[size.0 as f32, size.1 as f32, CORNER_RADIUS * scale, 0.0]));
                if let Some(target) = &self.target { draw_scene(f.queue, f.encoder, pipes, target, scene, &view, [r[2], r[3]], scale, look); }
                Some([r[0] * scale, r[1] * scale, size.0 as f32, size.1 as f32])
            }
            _ => None,
        };

        f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
        let device = f.device;
        let target = &self.target;
        f.ui.draw_with(f.encoder, f.target, clear, &mut |pass, site| {
            let (Some(rect), Some(target)) = (drawn, target) else { return };
            if site.mark != STAGE_MARK { return }
            let pipeline = pipes.blit(device, site.format).clone();
            pass.set_viewport(rect[0] - site.origin[0], rect[1] - site.origin[1], rect[2], rect[3], 0.0, 1.0);
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &target.group, &[]);
            pass.draw(0..3, 0..1);
        });
    }

    fn animating(&self) -> bool {
        self.moving
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_scene(queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, pipes: &Pipes, target: &Target, scene: &GpuScene, view: &View, stage: [f32; 2], scale: f32, look: Look) {
    let back = wgpu::Color { r: f64::from(look.backdrop[0]), g: f64::from(look.backdrop[1]), b: f64::from(look.backdrop[2]), a: 1.0 };
    match scene {
        GpuScene::Texture { size, groups } => {
            let fit = fit_scale(stage, *size);
            let uniform = TextureUniform {
                view: [target.size.0 as f32, target.size.1 as f32],
                size: [size.0 as f32, size.1 as f32],
                offset: [view.offset[0] * scale, view.offset[1] * scale],
                zoom: fit * view.zoom * scale,
                tile: f32::from(u8::from(view.tile)),
                seams: f32::from(u8::from(view.seams)),
                check: look.check,
                pad: 0.0,
                back: look.backdrop,
                end: 0.0,
            };
            queue.write_buffer(&pipes.texture_uniform, 0, bytemuck::bytes_of(&uniform));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("texture view"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(back), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipes.texture_pipe);
            pass.set_bind_group(0, &groups[usize::from(view.tile)][usize::from(view.smooth)], &[]);
            pass.draw(0..3, 0..1);
        }
        GpuScene::Model { vertices, indices, parts, marker, center, radius, .. } => {
            let turn = Mat4::from_rotation_x(view.pitch) * Mat4::from_rotation_y(view.yaw);
            let place = Mat4::from_translation(Vec3::new(view.pan[0] * radius, view.pan[1] * radius, -view.distance * radius));
            let aspect = target.size.0 as f32 / target.size.1.max(1) as f32;
            let near = (view.distance - 1.5).max(0.02) * radius;
            let proj = Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, near, (view.distance + 2.0) * radius);
            let clip = proj * place * turn * Mat4::from_translation(-*center);
            let light = LIGHT.normalize();
            let eye = (place * turn * Mat4::from_translation(-*center)).inverse().transform_point3(Vec3::ZERO);
            let uniform = ModelUniform { clip: clip.to_cols_array_2d(), light: [light.x, light.y, light.z, 0.0], eye: [eye.x, eye.y, eye.z, 0.0] };
            queue.write_buffer(&pipes.model_uniform, 0, bytemuck::bytes_of(&uniform));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model view"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.msaa,
                    depth_slice: None,
                    resolve_target: Some(&target.view),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(back), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipes.model_pipe);
            pass.set_bind_group(0, &pipes.global_group, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            for part in parts {
                pass.set_bind_group(1, &part.group, &[]);
                pass.draw_indexed(part.start..part.start + part.count, 0, 0..1);
            }
            if marker.visible {
                // Squeezed to the front of the depth range, so nothing can hide it.
                pass.set_viewport(0.0, 0.0, target.size.0 as f32, target.size.1 as f32, 0.0, 0.0005);
                pass.set_vertex_buffer(0, marker.vertices.slice(..));
                for (axis, group) in marker.groups.iter().enumerate() {
                    pass.set_bind_group(1, group, &[]);
                    pass.draw((axis * MARKER_AXIS_VERTICES) as u32..((axis + 1) * MARKER_AXIS_VERTICES) as u32, 0..1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_about_a_point_keeps_that_point_still() {
        let (at, offset, ratio) = ([40.0, -10.0], [5.0, 5.0], 2.5);
        let next = zoom_about(offset, at, ratio);
        // A point's distance from the pan scales by the zoom, so `at` stays where it was.
        assert!((at[0] - next[0] - (at[0] - offset[0]) * ratio).abs() < 1e-4);
        assert!((at[1] - next[1] - (at[1] - offset[1]) * ratio).abs() < 1e-4);
    }

    #[test]
    fn mips_run_down_to_a_single_texel_and_average() {
        let image = Image { width: 4, height: 2, rgba: [[0u8, 0, 0, 255], [200, 200, 200, 255]].concat().repeat(4) };
        let chain = mips(&image);
        assert_eq!(chain.iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(), [(4, 2), (2, 1), (1, 1)]);
        assert_eq!(chain[1].2[0], 100);
    }

    #[test]
    fn actual_size_shows_one_texel_per_device_pixel() {
        let zoom = actual_size_zoom([400.0, 400.0], (100, 100), 2.0);
        assert!((fit_scale([400.0, 400.0], (100, 100)) * zoom * 2.0 - 1.0).abs() < 1e-5);
    }
}
