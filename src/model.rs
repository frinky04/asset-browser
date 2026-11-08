//! Loads GLB, glTF, OBJ, DAE and FBX files into one renderable form.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::{Mat3, Mat4, Vec3};

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub color: [f32; 4],
    pub image: Option<u32>,
}

impl Default for Material {
    fn default() -> Self {
        Material { color: [0.72, 0.72, 0.72, 1.0], image: None }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub start: u32,
    pub count: u32,
    pub material: u32,
}

#[derive(Default)]
pub struct Model {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub parts: Vec<Part>,
    pub materials: Vec<Material>,
    pub images: Vec<Image>,
}

impl Model {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// Centre and radius of the bounding box.
    pub fn bounds(&self) -> (Vec3, f32) {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for v in &self.vertices {
            let p = Vec3::from(v.pos);
            min = min.min(p);
            max = max.max(p);
        }
        if self.vertices.is_empty() { return (Vec3::ZERO, 1.0); }
        ((min + max) * 0.5, ((max - min).length() * 0.5).max(1e-5))
    }

    /// Adds triangles for one material, filling in normals where the file has none.
    fn add(&mut self, mut vertices: Vec<Vertex>, mut indices: Vec<u32>, material: u32) {
        if indices.is_empty() { return; }
        if vertices.iter().all(|v| v.normal == [0.0; 3]) {
            let mut flat = Vec::with_capacity(indices.len());
            for tri in indices.chunks_exact(3) {
                let [a, b, c] = [vertices[tri[0] as usize], vertices[tri[1] as usize], vertices[tri[2] as usize]];
                let n = (Vec3::from(b.pos) - Vec3::from(a.pos)).cross(Vec3::from(c.pos) - Vec3::from(a.pos)).normalize_or_zero();
                for v in [a, b, c] { flat.push(Vertex { normal: n.into(), ..v }); }
            }
            indices = (0..flat.len() as u32).collect();
            vertices = flat;
        }
        let base = self.vertices.len() as u32;
        let start = self.indices.len() as u32;
        self.vertices.append(&mut vertices);
        self.indices.extend(indices.iter().map(|i| i + base));
        self.parts.push(Part { start, count: indices.len() as u32, material });
    }
}

pub fn decode_image(bytes: &[u8]) -> Option<Image> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(Image { width: img.width(), height: img.height(), rgba: img.into_raw() })
}

fn load_image_file(base: &Path, reference: &str, images: &mut Vec<Image>, seen: &mut HashMap<PathBuf, Option<u32>>) -> Option<u32> {
    let path = resolve(base, reference)?;
    if let Some(known) = seen.get(&path) { return *known; }
    let found = std::fs::read(&path).ok().and_then(|b| decode_image(&b)).map(|img| {
        images.push(img);
        images.len() as u32 - 1
    });
    seen.insert(path, found);
    found
}

/// Finds a file a model refers to: as written, or beside the model when the path is stale.
pub fn resolve(base: &Path, reference: &str) -> Option<PathBuf> {
    let cleaned = reference.trim().trim_start_matches("file:///").replace('\\', "/").replace("%20", " ");
    let direct = base.join(&cleaned);
    if direct.is_file() { return Some(direct); }
    let name = cleaned.rsplit('/').next()?;
    let beside = base.join(name);
    beside.is_file().then_some(beside)
}

pub fn load(path: &Path) -> Result<Model, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let model = match ext.as_str() {
        "glb" | "gltf" => load_gltf(path),
        "obj" => load_obj(path),
        "dae" => load_dae(path),
        "fbx" => load_fbx(path),
        other => Err(format!("{other} models aren't supported")),
    }?;
    if model.indices.is_empty() { return Err("the file has no triangles".into()); }
    Ok(model)
}

fn load_gltf(path: &Path) -> Result<Model, String> {
    let gltf::Gltf { document, blob } = gltf::Gltf::open(path).map_err(|e| e.to_string())?;
    let base = path.parent().unwrap_or(Path::new("."));
    let buffers = gltf::import_buffers(&document, Some(base), blob).map_err(|e| e.to_string())?;
    let mut model = Model::default();
    let mut image_ids: HashMap<usize, Option<u32>> = HashMap::new();
    let material_ids: Vec<u32> = document.materials().map(|m| {
        let pbr = m.pbr_metallic_roughness();
        let image = pbr.base_color_texture().and_then(|info| {
            let source = info.texture().source();
            *image_ids.entry(source.index()).or_insert_with(|| {
                let decoded = match source.source() {
                    gltf::image::Source::View { view, .. } => {
                        let data = &buffers[view.buffer().index()].0;
                        data.get(view.offset()..view.offset() + view.length()).and_then(decode_image)
                    }
                    gltf::image::Source::Uri { uri, .. } => resolve(base, uri).and_then(|p| std::fs::read(p).ok()).and_then(|b| decode_image(&b)),
                };
                decoded.map(|img| {
                    model.images.push(img);
                    model.images.len() as u32 - 1
                })
            })
        });
        model.materials.push(Material { color: pbr.base_color_factor(), image });
        model.materials.len() as u32 - 1
    }).collect();
    model.materials.push(Material::default());
    let fallback = model.materials.len() as u32 - 1;

    fn visit(node: gltf::Node<'_>, parent: Mat4, buffers: &[gltf::buffer::Data], ids: &[u32], fallback: u32, model: &mut Model) {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let normal_m = Mat3::from_mat4(world).inverse().transpose();
            for prim in mesh.primitives().filter(|p| p.mode() == gltf::mesh::Mode::Triangles) {
                let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                let Some(positions) = reader.read_positions() else { continue };
                let mut vertices: Vec<Vertex> = positions.map(|p| Vertex {
                    pos: world.transform_point3(Vec3::from(p)).into(),
                    normal: [0.0; 3],
                    uv: [0.0; 2],
                }).collect();
                if let Some(normals) = reader.read_normals() {
                    for (v, n) in vertices.iter_mut().zip(normals) { v.normal = (normal_m * Vec3::from(n)).normalize_or_zero().into(); }
                }
                if let Some(uvs) = reader.read_tex_coords(0) {
                    for (v, uv) in vertices.iter_mut().zip(uvs.into_f32()) { v.uv = uv; }
                }
                let indices = reader.read_indices().map_or_else(|| (0..vertices.len() as u32).collect(), |i| i.into_u32().collect());
                let material = prim.material().index().and_then(|i| ids.get(i).copied()).unwrap_or(fallback);
                model.add(vertices, indices, material);
            }
        }
        for child in node.children() { visit(child, world, buffers, ids, fallback, model); }
    }
    let scene = document.default_scene().or_else(|| document.scenes().next()).ok_or("the file has no scene")?;
    for node in scene.nodes() { visit(node, Mat4::IDENTITY, &buffers, &material_ids, fallback, &mut model); }
    Ok(model)
}

fn load_obj(path: &Path) -> Result<Model, String> {
    let options = tobj::LoadOptions { triangulate: true, single_index: true, ignore_points: true, ignore_lines: true };
    let (meshes, materials) = tobj::load_obj(path, &options).map_err(|e| e.to_string())?;
    let materials = materials.unwrap_or_default();
    let base = path.parent().unwrap_or(Path::new("."));
    let mut model = Model::default();
    let mut seen = HashMap::new();
    for m in &materials {
        let image = m.diffuse_texture.as_deref().and_then(|t| load_image_file(base, t, &mut model.images, &mut seen));
        let d = m.diffuse.unwrap_or([1.0, 1.0, 1.0]);
        model.materials.push(Material { color: if image.is_some() { [1.0; 4] } else { [d[0], d[1], d[2], 1.0] }, image });
    }
    model.materials.push(Material::default());
    let fallback = model.materials.len() as u32 - 1;
    for mesh in meshes.iter().map(|m| &m.mesh) {
        let vertices = (0..mesh.positions.len() / 3).map(|i| {
            let p = &mesh.positions[i * 3..i * 3 + 3];
            let normal = mesh.normals.get(i * 3..i * 3 + 3).map_or([0.0; 3], |n| [n[0], n[1], n[2]]);
            let uv = mesh.texcoords.get(i * 2..i * 2 + 2).map_or([0.0; 2], |t| [t[0], 1.0 - t[1]]);
            Vertex { pos: [p[0], p[1], p[2]], normal, uv }
        }).collect();
        model.add(vertices, mesh.indices.clone(), mesh.material_id.map_or(fallback, |i| i as u32));
    }
    Ok(model)
}

fn parse_floats(text: &str) -> Vec<f32> {
    text.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect()
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name(name))
}

fn fragment(url: &str) -> &str {
    url.strip_prefix('#').unwrap_or(url)
}

/// A COLLADA node's local transform: its `matrix`, `translate`, `rotate` and `scale` children in order.
fn dae_transform(node: roxmltree::Node<'_, '_>) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    for c in node.children().filter(|c| c.is_element()) {
        let v = parse_floats(c.text().unwrap_or(""));
        let step = match (c.tag_name().name(), v.len()) {
            ("matrix", 16) => Mat4::from_cols_slice(&v).transpose(),
            ("translate", 3) => Mat4::from_translation(Vec3::new(v[0], v[1], v[2])),
            ("rotate", 4) => Mat4::from_axis_angle(Vec3::new(v[0], v[1], v[2]).normalize_or_zero(), v[3].to_radians()),
            ("scale", 3) => Mat4::from_scale(Vec3::new(v[0], v[1], v[2])),
            _ => continue,
        };
        m *= step;
    }
    m
}

fn load_dae(path: &Path) -> Result<Model, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let doc = roxmltree::Document::parse(&text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let base = path.parent().unwrap_or(Path::new("."));
    let mut model = Model::default();
    let mut seen = HashMap::new();

    let images: HashMap<&str, &str> = root.descendants().filter(|n| n.has_tag_name("image"))
        .filter_map(|n| Some((n.attribute("id")?, n.descendants().find(|c| c.has_tag_name("init_from"))?.text()?.trim())))
        .collect();

    // Effects name a sampler, which names a surface, which names an image.
    let mut effect_materials: HashMap<&str, Material> = HashMap::new();
    for effect in root.descendants().filter(|n| n.has_tag_name("effect")) {
        let Some(id) = effect.attribute("id") else { continue };
        let mut params: HashMap<&str, &str> = HashMap::new();
        for p in effect.descendants().filter(|n| n.has_tag_name("newparam")) {
            let Some(sid) = p.attribute("sid") else { continue };
            let value = p.descendants().find(|c| c.has_tag_name("init_from") || c.has_tag_name("source")).and_then(|c| c.text());
            if let Some(value) = value { params.insert(sid, value.trim()); }
        }
        let mut material = Material::default();
        if let Some(diffuse) = effect.descendants().find(|n| n.has_tag_name("diffuse")) {
            if let Some(t) = child(diffuse, "texture").and_then(|t| t.attribute("texture")) {
                let mut name = t;
                for _ in 0..3 { if let Some(next) = params.get(name) { name = next; } }
                let reference = images.get(name).copied().unwrap_or(name);
                material.image = load_image_file(base, reference, &mut model.images, &mut seen);
                if material.image.is_some() { material.color = [1.0; 4]; }
            } else if let Some(c) = child(diffuse, "color").and_then(|c| c.text()) {
                let v = parse_floats(c);
                if v.len() >= 3 { material.color = [v[0], v[1], v[2], 1.0]; }
            }
        }
        effect_materials.insert(id, material);
    }
    let materials: HashMap<&str, Material> = root.descendants().filter(|n| n.has_tag_name("material"))
        .filter_map(|n| {
            let id = n.attribute("id")?;
            let effect = fragment(child(n, "instance_effect")?.attribute("url")?);
            Some((id, *effect_materials.get(effect)?))
        }).collect();

    let geometries: HashMap<&str, roxmltree::Node<'_, '_>> = root.descendants().filter(|n| n.has_tag_name("geometry"))
        .filter_map(|n| Some((n.attribute("id")?, child(n, "mesh")?))).collect();

    let z_up = root.descendants().find(|n| n.has_tag_name("up_axis")).and_then(|n| n.text()).is_some_and(|t| t.trim() == "Z_UP");
    let top = if z_up { Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Mat4::IDENTITY };

    fn visit<'a>(
        node: roxmltree::Node<'a, 'a>,
        parent: Mat4,
        geometries: &HashMap<&'a str, roxmltree::Node<'a, 'a>>,
        materials: &HashMap<&'a str, Material>,
        model: &mut Model,
    ) {
        let world = parent * dae_transform(node);
        for c in node.children().filter(|c| c.is_element()) {
            if c.has_tag_name("node") { visit(c, world, geometries, materials, model); }
            if !c.has_tag_name("instance_geometry") { continue }
            let Some(mesh) = c.attribute("url").and_then(|u| geometries.get(fragment(u))) else { continue };
            let bound: HashMap<&str, &str> = c.descendants().filter(|n| n.has_tag_name("instance_material"))
                .filter_map(|n| Some((n.attribute("symbol")?, fragment(n.attribute("target")?)))).collect();
            add_dae_mesh(*mesh, world, &bound, materials, model);
        }
    }
    let scene = root.descendants().find(|n| n.has_tag_name("visual_scene")).ok_or("the file has no scene")?;
    for n in scene.children().filter(|n| n.has_tag_name("node")) { visit(n, top, &geometries, &materials, &mut model); }
    Ok(model)
}

fn add_dae_mesh(mesh: roxmltree::Node<'_, '_>, world: Mat4, bound: &HashMap<&str, &str>, materials: &HashMap<&str, Material>, model: &mut Model) {
    let sources: HashMap<&str, (Vec<f32>, usize)> = mesh.children().filter(|n| n.has_tag_name("source"))
        .filter_map(|s| {
            let data = parse_floats(child(s, "float_array")?.text()?);
            let stride = s.descendants().find(|n| n.has_tag_name("accessor")).and_then(|a| a.attribute("stride")).and_then(|v| v.parse().ok()).unwrap_or(3);
            Some((s.attribute("id")?, (data, stride)))
        }).collect();
    let vertices_input: HashMap<&str, &str> = mesh.children().filter(|n| n.has_tag_name("vertices"))
        .filter_map(|v| {
            let position = v.children().find(|i| i.has_tag_name("input") && i.attribute("semantic") == Some("POSITION"))?;
            Some((v.attribute("id")?, fragment(position.attribute("source")?)))
        }).collect();
    let normal_m = Mat3::from_mat4(world).inverse().transpose();

    for prim in mesh.children().filter(|n| n.has_tag_name("triangles") || n.has_tag_name("polylist")) {
        let Some(p) = child(prim, "p").and_then(|p| p.text()) else { continue };
        let indices: Vec<usize> = p.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
        let (mut pos, mut norm, mut uv) = (None, None, None);
        let mut width = 0;
        for input in prim.children().filter(|n| n.has_tag_name("input")) {
            let offset: usize = input.attribute("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
            width = width.max(offset + 1);
            let source = fragment(input.attribute("source").unwrap_or(""));
            match input.attribute("semantic") {
                Some("VERTEX") => pos = vertices_input.get(source).and_then(|s| sources.get(s)).map(|s| (s, offset)),
                Some("NORMAL") => norm = sources.get(source).map(|s| (s, offset)),
                Some("TEXCOORD") if uv.is_none() => uv = sources.get(source).map(|s| (s, offset)),
                _ => {}
            }
        }
        let Some(((positions, pos_stride), pos_offset)) = pos else { continue };
        if width == 0 { continue }
        let corners = indices.len() / width;
        let mut vertices = Vec::with_capacity(corners);
        for c in 0..corners {
            let at = |offset: usize| indices[c * width + offset];
            let Some(p) = positions.get(at(pos_offset) * pos_stride..at(pos_offset) * pos_stride + 3) else { continue };
            let normal = norm.and_then(|((d, s), o)| d.get(at(o) * s..at(o) * s + 3))
                .map_or([0.0; 3], |n| (normal_m * Vec3::new(n[0], n[1], n[2])).normalize_or_zero().into());
            let uv = uv.and_then(|((d, s), o)| d.get(at(o) * s..at(o) * s + 2)).map_or([0.0; 2], |t| [t[0], 1.0 - t[1]]);
            vertices.push(Vertex { pos: world.transform_point3(Vec3::new(p[0], p[1], p[2])).into(), normal, uv });
        }
        // Polylists are fans of each polygon's `vcount` corners.
        let mut tris = Vec::with_capacity(vertices.len());
        match child(prim, "vcount").and_then(|v| v.text()) {
            Some(counts) => {
                let mut start = 0u32;
                for n in counts.split_ascii_whitespace().filter_map(|t| t.parse::<u32>().ok()) {
                    for i in 1..n.saturating_sub(1) { tris.extend([start, start + i, start + i + 1]); }
                    start += n;
                }
            }
            None => tris.extend(0..vertices.len() as u32),
        }
        tris.retain(|i| (*i as usize) < vertices.len());
        tris.truncate(tris.len() / 3 * 3);
        let material = prim.attribute("material").and_then(|m| bound.get(m)).and_then(|t| materials.get(t)).copied().unwrap_or_default();
        model.materials.push(material);
        let id = model.materials.len() as u32 - 1;
        model.add(vertices, tris, id);
    }
}

/// Reads from memory so a path that isn't valid UTF-8 still opens.
fn parse_fbx(path: &Path, opts: ufbx::LoadOpts<'_>) -> Result<ufbx::SceneRoot, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    ufbx::load_memory(&bytes, opts).map_err(|e| e.description.to_string())
}

/// Where a texture an FBX names lives on disk. The relative path is the one that survives a
/// pack being moved; the absolute one usually points at the artist's machine.
fn fbx_texture_path(base: &Path, file: &ufbx::TextureFile) -> Option<PathBuf> {
    let references = [&file.relative_filename, &file.absolute_filename, &file.filename];
    references.iter().find_map(|r| resolve(base, r)).or_else(|| references.iter().find_map(|r| find_in_parents(base, r)))
}

/// A path from the artist's machine still ends in the pack's own layout, like
/// `Textures/wood.png`, and that folder is usually somewhere above the model.
fn find_in_parents(base: &Path, reference: &str) -> Option<PathBuf> {
    const LEVELS: usize = 6;
    let cleaned = reference.replace('\\', "/");
    let mut parts = cleaned.rsplit('/');
    let (name, folder) = (parts.next()?, parts.next()?);
    base.ancestors().take(LEVELS).map(|dir| dir.join(folder).join(name)).find(|p| p.is_file())
}

/// The files on disk an FBX's textures come from. Embedded ones travel inside the FBX.
pub fn fbx_texture_files(path: &Path) -> Vec<PathBuf> {
    let Ok(scene) = parse_fbx(path, ufbx::LoadOpts { ignore_geometry: true, ignore_animation: true, ..Default::default() }) else { return Vec::new() };
    let base = path.parent().unwrap_or(Path::new("."));
    scene.texture_files.iter().filter(|f| f.content.is_empty()).filter_map(|f| fbx_texture_path(base, f)).collect()
}

fn load_fbx(path: &Path) -> Result<Model, String> {
    let scene = parse_fbx(path, ufbx::LoadOpts {
        ignore_animation: true,
        generate_missing_normals: true,
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        ..Default::default()
    })?;
    let base = path.parent().unwrap_or(Path::new("."));
    let mut model = Model::default();
    let mut image_ids: HashMap<u32, Option<u32>> = HashMap::new();
    // Pushed in order, so a material's `typed_id` is also its index in `model.materials`.
    for m in scene.materials.iter() {
        let pbr = &m.pbr;
        let file = pbr.base_color.texture.as_ref().or(m.fbx.diffuse_color.texture.as_ref()).and_then(|t| t.file_textures.first());
        let image = file.and_then(|f| *image_ids.entry(f.file_index).or_insert_with(|| {
            let source = scene.texture_files.get(f.file_index as usize)?;
            let decoded = decode_image(&source.content)
                .or_else(|| fbx_texture_path(base, source).and_then(|p| std::fs::read(p).ok()).and_then(|b| decode_image(&b)));
            decoded.map(|img| {
                model.images.push(img);
                model.images.len() as u32 - 1
            })
        }));
        let mut material = Material { image, ..Material::default() };
        if image.is_some() {
            material.color = [1.0; 4];
        } else if pbr.base_color.has_value {
            let factor = if pbr.base_factor.has_value { pbr.base_factor.value_vec4.x } else { 1.0 };
            let c = pbr.base_color.value_vec4;
            material.color = [(c.x * factor) as f32, (c.y * factor) as f32, (c.z * factor) as f32, 1.0];
        }
        model.materials.push(material);
    }
    model.materials.push(Material::default());
    let fallback = model.materials.len() as u32 - 1;
    for node in scene.nodes.iter().filter(|n| n.visible) {
        if let Some(mesh) = node.mesh.as_ref() { add_fbx_mesh(node, mesh, fallback, &mut model); }
    }
    Ok(model)
}

fn add_fbx_mesh(node: &ufbx::Node, mesh: &ufbx::Mesh, fallback: u32, model: &mut Model) {
    let m = &node.geometry_to_world;
    let world = Mat4::from_cols_array(&[m.m00, m.m10, m.m20, 0.0, m.m01, m.m11, m.m21, 0.0, m.m02, m.m12, m.m22, 0.0, m.m03, m.m13, m.m23, 1.0].map(|v| v as f32));
    let normal_m = Mat3::from_mat4(world).inverse().transpose();
    let mut corners = vec![0u32; mesh.max_face_triangles * 3];
    for part in mesh.material_parts.iter() {
        let mut vertices = Vec::with_capacity(part.num_triangles * 3);
        for face in part.face_indices.iter().filter_map(|f| mesh.faces.get(*f as usize)) {
            let triangles = mesh.triangulate_face(&mut corners, *face) as usize;
            for &corner in &corners[..triangles * 3] {
                let corner = corner as usize;
                let p = mesh.vertex_position[corner];
                let normal = if mesh.vertex_normal.exists {
                    let n = mesh.vertex_normal[corner];
                    (normal_m * Vec3::new(n.x as f32, n.y as f32, n.z as f32)).normalize_or_zero().into()
                } else {
                    [0.0; 3]
                };
                let uv = if mesh.vertex_uv.exists {
                    let t = mesh.vertex_uv[corner];
                    [t.x as f32, 1.0 - t.y as f32]
                } else {
                    [0.0; 2]
                };
                vertices.push(Vertex { pos: world.transform_point3(Vec3::new(p.x as f32, p.y as f32, p.z as f32)).into(), normal, uv });
            }
        }
        let indices = (0..vertices.len() as u32).collect();
        // A node can override the materials of the mesh it instances.
        let index = part.index as usize;
        let material = node.materials.get(index).or(mesh.materials.get(index)).map_or(fallback, |m| m.element.typed_id);
        model.add(vertices, indices, material);
    }
}

/// A Z-up quad whose texture is named by a path relative to the file and a dead absolute one.
#[cfg(test)]
pub(crate) const FBX_QUAD: &str = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
    FBXHeaderVersion: 1003
    FBXVersion: 7400
}
GlobalSettings:  {
    Version: 1000
    Properties70:  {
        P: "UpAxis", "int", "Integer", "",2
        P: "UpAxisSign", "int", "Integer", "",1
        P: "FrontAxis", "int", "Integer", "",1
        P: "FrontAxisSign", "int", "Integer", "",-1
        P: "CoordAxis", "int", "Integer", "",0
        P: "CoordAxisSign", "int", "Integer", "",1
    }
}
Objects:  {
    Geometry: 1, "Geometry::quad", "Mesh" {
        Vertices: *12 { a: 0,0,0,1,0,0,1,1,0,0,1,0 }
        PolygonVertexIndex: *4 { a: 0,1,2,-4 }
        GeometryVersion: 124
        LayerElementUV: 0 {
            Version: 101
            Name: "uv"
            MappingInformationType: "ByPolygonVertex"
            ReferenceInformationType: "Direct"
            UV: *8 { a: 0,0,1,0,1,1,0,1 }
        }
        Layer: 0 {
            Version: 100
            LayerElement:  {
                Type: "LayerElementUV"
                TypedIndex: 0
            }
        }
    }
    Model: 2, "Model::quad", "Mesh" {
        Version: 232
    }
    Material: 3, "Material::red", "" {
        Version: 102
        ShadingModel: "lambert"
        Properties70:  {
            P: "DiffuseColor", "Color", "", "A",1,0,0
        }
    }
    Texture: 4, "Texture::wood", "" {
        Type: "TextureVideoClip"
        Version: 202
        FileName: "C:/gone/artist/wood.png"
        RelativeFilename: "tex/wood.png"
    }
}
Connections:  {
    C: "OO",1,2
    C: "OO",2,0
    C: "OO",3,2
    C: "OP",4,3,"DiffuseColor"
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const DAE: &str = r##"<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema">
      <asset><up_axis>Z_UP</up_axis></asset>
      <library_geometries><geometry id="g"><mesh>
        <source id="p"><float_array>0 0 0 1 0 0 1 1 0 0 1 0</float_array><technique_common><accessor stride="3"/></technique_common></source>
        <vertices id="v"><input semantic="POSITION" source="#p"/></vertices>
        <polylist count="1"><input semantic="VERTEX" source="#v" offset="0"/><vcount>4</vcount><p>0 1 2 3</p></polylist>
      </mesh></geometry></library_geometries>
      <library_visual_scenes><visual_scene id="s"><node><instance_geometry url="#g"/></node></visual_scene></library_visual_scenes>
    </COLLADA>"##;

    #[test]
    fn a_dae_polygon_becomes_a_fan_and_z_up_turns_to_y_up() {
        let dir = std::env::temp_dir().join(format!("ab-dae-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("quad.dae");
        std::fs::write(&file, DAE).unwrap();
        let model = load(&file).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(model.triangles(), 2);
        let top = model.vertices.iter().map(|v| v.pos[2]).fold(f32::MAX, f32::min);
        assert!((top + 1.0).abs() < 1e-5, "y became -z: {top}");
    }

    fn fbx_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ab-fbx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        std::fs::write(dir.join("quad.fbx"), FBX_QUAD).unwrap();
        dir
    }

    #[test]
    fn an_fbx_polygon_triangulates_turns_y_up_and_finds_its_texture() {
        let dir = fbx_dir("textured");
        image::RgbaImage::new(2, 2).save(dir.join("tex/wood.png")).unwrap();
        let model = load(&dir.join("quad.fbx")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(model.triangles(), 2);
        let top = model.vertices.iter().map(|v| v.pos[2]).fold(f32::MAX, f32::min);
        assert!((top + 1.0).abs() < 1e-5, "y became -z: {top}");
        assert_eq!(model.images.len(), 1);
        assert_eq!(model.materials[model.parts[0].material as usize].image, Some(0));
        let corner = model.vertices.iter().find(|v| v.pos == [0.0; 3]).unwrap();
        assert_eq!(corner.uv, [0.0, 1.0], "v is flipped to match the renderer");
        assert!(corner.normal != [0.0; 3], "missing normals are generated");
    }

    #[test]
    fn an_fbx_with_a_missing_texture_falls_back_to_its_colour() {
        let dir = fbx_dir("untextured");
        let model = load(&dir.join("quad.fbx")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let material = model.materials[model.parts[0].material as usize];
        assert_eq!(material.image, None);
        assert_eq!(material.color, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn an_fbx_with_a_stale_path_finds_its_texture_in_a_folder_above_it() {
        let dir = fbx_dir("stale");
        image::RgbaImage::new(2, 2).save(dir.join("tex/wood.png")).unwrap();
        let text = FBX_QUAD.replace("tex/wood.png", "C:/gone/artist/tex/wood.png");
        std::fs::create_dir_all(dir.join("Models/FBX")).unwrap();
        std::fs::write(dir.join("Models/FBX/quad.fbx"), text).unwrap();
        let model = load(&dir.join("Models/FBX/quad.fbx")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(model.images.len(), 1);
    }

    #[test]
    fn an_embedded_fbx_texture_decodes_and_stays_inside_the_file() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let dir = fbx_dir("embedded");
        let video = format!("    Video: 5, \"Video::wood\", \"Clip\" {{\n        Type: \"Clip\"\n        Filename: \"C:/gone/artist/tex/wood.png\"\n        RelativeFilename: \"tex/wood.png\"\n        Content: , \"{PNG}\"\n    }}\n}}\nConnections:");
        let text = FBX_QUAD.replace("}\nConnections:", &video).replace("C: \"OO\",1,2", "C: \"OO\",1,2\n    C: \"OO\",5,4");
        std::fs::write(dir.join("quad.fbx"), text).unwrap();
        let model = load(&dir.join("quad.fbx")).unwrap();
        let on_disk = fbx_texture_files(&dir.join("quad.fbx"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((model.images.len(), model.images[0].width), (1, 1));
        assert!(on_disk.is_empty());
    }

    #[test]
    fn a_broken_fbx_is_an_error_and_not_a_crash() {
        let dir = fbx_dir("broken");
        std::fs::write(dir.join("quad.fbx"), b"Kaydara FBX Binary  \x00\x1a\x00garbage").unwrap();
        assert!(load(&dir.join("quad.fbx")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
