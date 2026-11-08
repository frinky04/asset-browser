//! Material edits, and writing models out as GLB. A glTF source keeps its whole document (nodes,
//! skins, animation, extensions) and only has its materials and images changed; anything else
//! becomes a static document built from the mesh the preview already has. Both then go through
//! the same edit and write steps.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use serde_json::{Value, json};

use super::picture::{self, ImageEdit};
use crate::model::{self, Image, Model};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialEdit {
    pub color: Option<[f32; 3]>,
    pub roughness: Option<f32>,
    pub metallic: Option<f32>,
    /// The brightest emissive channel.
    pub emissive: Option<f32>,
    /// How strongly the normal map bends the surface.
    pub normal: Option<f32>,
    /// The largest side this material's textures leave at.
    pub max_texture: Option<u32>,
}

/// How a texture is sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFilter {
    Smooth,
    Pixels,
}

impl TextureFilter {
    pub fn title(self) -> &'static str {
        match self {
            TextureFilter::Smooth => "Smooth",
            TextureFilter::Pixels => "Pixels",
        }
    }

    /// glTF's magnification and minification codes. Pixels keeps its mipmaps, so it stays crisp
    /// up close without sparkling in the distance.
    fn codes(self) -> (u32, u32) {
        match self {
            TextureFilter::Smooth => (9729, 9987),
            TextureFilter::Pixels => (9728, 9984),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelEdit {
    pub materials: BTreeMap<usize, MaterialEdit>,
    /// The filter for every texture, unless `textures` says otherwise.
    pub filter: Option<TextureFilter>,
    /// Filters for particular images, by document image.
    pub textures: BTreeMap<usize, TextureFilter>,
    /// Normal maps to convert between DirectX and OpenGL, by document image.
    pub flip_normal: BTreeSet<usize>,
    /// Where the model's origin moves to.
    pub origin: Option<Origin>,
    /// A uniform scale for the whole model.
    pub scale: Option<f32>,
}

/// A place to put a model's origin, worked out from its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Centre,
    Base,
}

impl ModelEdit {
    pub fn is_unchanged(&self) -> bool {
        self.filter.is_none() && self.textures.is_empty() && self.flip_normal.is_empty() && self.origin.is_none() && self.scale.is_none()
            && self.materials.values().all(|m| *m == MaterialEdit::default())
    }

    /// An edit of just one material.
    pub fn single(material: usize, change: MaterialEdit) -> ModelEdit {
        ModelEdit { materials: [(material, change)].into(), ..ModelEdit::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Base,
    MetalRough,
    Normal,
    Occlusion,
    Emissive,
}

impl Slot {
    pub fn title(self) -> &'static str {
        match self {
            Slot::Base => "Base colour",
            Slot::MetalRough => "Metal / rough",
            Slot::Normal => "Normal",
            Slot::Occlusion => "Occlusion",
            Slot::Emissive => "Emissive",
        }
    }

    /// A label that fits under a small thumbnail.
    pub fn short(self) -> &'static str {
        match self {
            Slot::Base => "Base",
            Slot::MetalRough => "M / R",
            Slot::Normal => "Normal",
            Slot::Occlusion => "AO",
            Slot::Emissive => "Glow",
        }
    }
}

/// One material, as the file has it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialInfo {
    pub name: String,
    pub color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub emissive: [f32; 3],
    pub normal_scale: f32,
    pub maps: Vec<(Slot, usize)>,
}

impl MaterialInfo {
    pub fn map(&self, slot: Slot) -> Option<usize> {
        self.maps.iter().find(|(s, _)| *s == slot).map(|(_, i)| *i)
    }

    /// Whether emission means anything here.
    pub fn glows(&self) -> bool {
        self.map(Slot::Emissive).is_some() || self.emissive.iter().any(|c| *c > 0.0)
    }
}

/// How one material looks in the preview. Indexed like `Model::materials`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceMaterial {
    pub color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub emissive: [f32; 3],
    pub image: Option<u32>,
    pub surface_image: Option<u32>,
    pub normal_image: Option<u32>,
    pub emissive_image: Option<u32>,
    pub normal_scale: f32,
    /// The document material this is, if any.
    pub doc: Option<u32>,
    /// Filters for the base, metal/rough, normal and emissive maps. `None` is unspecified.
    pub filters: [Option<TextureFilter>; 4],
}

/// What the preview draws a model's materials with.
#[derive(Clone)]
pub struct Surface {
    pub materials: Vec<SurfaceMaterial>,
    pub images: Vec<Arc<Image>>,
    /// The document material to pick out, leaving the rest dim.
    pub focus: Option<usize>,
    /// Where to mark the model's origin, if at all.
    pub origin: Option<[f32; 3]>,
}

struct DocImage {
    /// The file's own bytes and type, kept so an image nobody resized leaves untouched.
    encoded: Option<(Vec<u8>, String)>,
    pixels: OnceLock<Option<Arc<Image>>>,
    size: Option<(u32, u32)>,
    view: Option<usize>,
}

impl DocImage {
    fn from_bytes(bytes: Vec<u8>, stated_mime: Option<&str>, view: Option<usize>) -> DocImage {
        let mime = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Png) => "image/png".to_owned(),
            Ok(image::ImageFormat::Jpeg) => "image/jpeg".to_owned(),
            _ => stated_mime.unwrap_or("application/octet-stream").to_owned(),
        };
        let size = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
        DocImage { encoded: Some((bytes, mime)), pixels: OnceLock::new(), size, view }
    }

    fn from_pixels(pixels: Arc<Image>) -> DocImage {
        let size = Some((pixels.width, pixels.height));
        let lock = OnceLock::new();
        let _ = lock.set(Some(pixels));
        DocImage { encoded: None, pixels: lock, size, view: None }
    }

    fn pixels(&self) -> Option<Arc<Image>> {
        self.pixels.get_or_init(|| self.encoded.as_ref().and_then(|(bytes, _)| model::decode_image(bytes)).map(Arc::new)).clone()
    }

    /// Only PNG and JPEG can be rebuilt and still be valid core glTF.
    fn resizable(&self) -> bool {
        self.size.is_some() && self.encoded.as_ref().is_none_or(|(_, mime)| mime == "image/png" || mime == "image/jpeg")
    }

    fn is_jpeg(&self) -> bool {
        self.encoded.as_ref().is_some_and(|(_, mime)| mime == "image/jpeg")
    }
}

struct Rebuilt {
    pixels: Arc<Image>,
    bytes: Vec<u8>,
    mime: &'static str,
}

/// A model as a glTF document with a single binary buffer, ready to edit and write.
pub struct ModelDoc {
    root: Value,
    buffer: Vec<u8>,
    images: Vec<DocImage>,
    pub materials: Vec<MaterialInfo>,
    /// For each of `Model::materials`, the document material it is.
    flat: Vec<Option<usize>>,
    /// What the file asks for, per image, where it says.
    image_filters: Vec<Option<TextureFilter>>,
    /// Slow to make, and a material slider shouldn't redo them. Bounded by the sizes on offer.
    rebuilt: Mutex<HashMap<(usize, u32, bool, bool), Arc<Rebuilt>>>,
    /// The lowest and highest corners of the model as the file places it.
    bounds: ([f32; 3], [f32; 3]),
    /// Whether this is a whole glTF document, rather than a flattened mesh.
    pub keeps_scene: bool,
}

pub fn open(path: &Path, library: &Path) -> Result<ModelDoc, String> {
    let model = model::load(path, library)?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (mut low, mut high) = ([f32::MAX; 3], [f32::MIN; 3]);
    for vertex in &model.vertices {
        for axis in 0..3 {
            low[axis] = low[axis].min(vertex.pos[axis]);
            high[axis] = high[axis].max(vertex.pos[axis]);
        }
    }
    let mut doc = if matches!(ext.as_str(), "glb" | "gltf") { from_gltf(path, model)? } else { from_flat(model) };
    doc.bounds = (low, high);
    // Decoded now, off the UI thread, because the preview's shading uses them straight away.
    for material in &doc.materials {
        for (_, image) in &material.maps { doc.images[*image].pixels(); }
    }
    Ok(doc)
}

fn texture_image(root: &Value, info: Option<&Value>) -> Option<usize> {
    let index = info?.get("index")?.as_u64()? as usize;
    Some(root.get("textures")?.get(index)?.get("source")?.as_u64()? as usize)
}

fn array3(value: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    value.and_then(Value::as_array).filter(|a| a.len() >= 3).map_or(default, |a| [0, 1, 2].map(|i| a[i].as_f64().unwrap_or(f64::from(default[i])) as f32))
}

fn read_materials(root: &Value) -> Vec<MaterialInfo> {
    let list = root.get("materials").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    list.iter().enumerate().map(|(i, m)| {
        let pbr = m.get("pbrMetallicRoughness");
        let factor = |name: &str, default: f32| pbr.and_then(|p| p.get(name)).and_then(Value::as_f64).map_or(default, |v| v as f32);
        let color = pbr.and_then(|p| p.get("baseColorFactor")).and_then(Value::as_array).filter(|a| a.len() == 4)
            .map_or([1.0; 4], |a| [0, 1, 2, 3].map(|c| a[c].as_f64().unwrap_or(1.0) as f32));
        let mut maps = Vec::new();
        for (slot, info) in [
            (Slot::Base, pbr.and_then(|p| p.get("baseColorTexture"))),
            (Slot::MetalRough, pbr.and_then(|p| p.get("metallicRoughnessTexture"))),
            (Slot::Normal, m.get("normalTexture")),
            (Slot::Occlusion, m.get("occlusionTexture")),
            (Slot::Emissive, m.get("emissiveTexture")),
        ] {
            if let Some(image) = texture_image(root, info) { maps.push((slot, image)); }
        }
        MaterialInfo {
            name: m.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).map_or_else(|| format!("Material {}", i + 1), str::to_owned),
            color,
            roughness: factor("roughnessFactor", 1.0),
            metallic: factor("metallicFactor", 1.0),
            emissive: array3(m.get("emissiveFactor"), [0.0; 3]),
            normal_scale: m.get("normalTexture").and_then(|n| n.get("scale")).and_then(Value::as_f64).map_or(1.0, |v| v as f32),
            maps,
        }
    }).collect()
}

fn from_gltf(path: &Path, model: Model) -> Result<ModelDoc, String> {
    let base = path.parent().unwrap_or(Path::new("."));
    let file = std::fs::read(path).map_err(|e| e.to_string())?;
    let (mut root, blob): (Value, Option<Vec<u8>>) = if file.starts_with(b"glTF") {
        let glb = gltf::binary::Glb::from_slice(&file).map_err(|e| e.to_string())?;
        (serde_json::from_slice(&glb.json).map_err(|e| e.to_string())?, glb.bin.map(std::borrow::Cow::into_owned))
    } else {
        (serde_json::from_slice(&file).map_err(|e| e.to_string())?, None)
    };
    let document = gltf::Gltf::from_slice(&file).map_err(|e| e.to_string())?.document;
    let sources = gltf::import_buffers(&document, Some(base), blob).map_err(|e| e.to_string())?;

    // Every buffer becomes one, so offsets only ever mean one thing.
    let mut buffer = Vec::new();
    let mut starts = Vec::with_capacity(sources.len());
    for source in &sources {
        buffer.resize(buffer.len().next_multiple_of(8), 0);
        starts.push(buffer.len());
        buffer.extend_from_slice(&source.0);
    }
    if let Some(views) = root.get_mut("bufferViews").and_then(Value::as_array_mut) {
        for view in views {
            let from = view.get("buffer").and_then(Value::as_u64).unwrap_or(0) as usize;
            let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
            view["buffer"] = json!(0);
            view["byteOffset"] = json!(offset + starts.get(from).copied().unwrap_or(0));
        }
    }

    let mut images = Vec::new();
    for entry in root.get("images").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        let mime = entry.get("mimeType").and_then(Value::as_str);
        let view = entry.get("bufferView").and_then(Value::as_u64).map(|v| v as usize);
        let bytes = if let Some(v) = view {
            root.get("bufferViews").and_then(|views| views.get(v)).and_then(|view| {
                let (offset, length) = (view.get("byteOffset")?.as_u64()? as usize, view.get("byteLength")?.as_u64()? as usize);
                buffer.get(offset..offset + length).map(<[u8]>::to_vec)
            })
        } else {
            entry.get("uri").and_then(Value::as_str).and_then(|uri| read_uri(base, uri))
        };
        images.push(match bytes {
            Some(bytes) => DocImage::from_bytes(bytes, mime, view),
            // Left as it was: nothing to embed, and nothing better to say about it.
            None => DocImage { encoded: None, pixels: OnceLock::from(None), size: None, view },
        });
    }

    adopt_guesses(&mut root, &mut images, &model);
    let materials = read_materials(&root);
    let flat = (0..model.materials.len()).map(|i| (i < materials.len()).then_some(i)).collect();
    let image_filters = read_filters(&root, images.len());
    Ok(ModelDoc { root, buffer, images, materials, flat, image_filters, rebuilt: Mutex::default(), keeps_scene: true, bounds: ([0.0; 3], [0.0; 3]) })
}

/// What each image's texture asks for, going by its magnification filter.
fn read_filters(root: &Value, images: usize) -> Vec<Option<TextureFilter>> {
    let mut filters = vec![None; images];
    for texture in root.get("textures").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        let Some(image) = texture.get("source").and_then(Value::as_u64) else { continue };
        let sampler = texture.get("sampler").and_then(Value::as_u64).and_then(|s| root.get("samplers")?.get(s as usize));
        let found = match sampler.and_then(|s| s.get("magFilter")).and_then(Value::as_u64) {
            Some(9728) => Some(TextureFilter::Pixels),
            Some(9729) => Some(TextureFilter::Smooth),
            _ => None,
        };
        if let Some(slot) = filters.get_mut(image as usize) && slot.is_none() { *slot = found; }
    }
    filters
}

fn read_uri(base: &Path, uri: &str) -> Option<Vec<u8>> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (_, encoded) = data.split_once(";base64,")?;
        return base64::engine::general_purpose::STANDARD.decode(encoded).ok();
    }
    std::fs::read(model::resolve(base, uri)?).ok()
}

/// Puts textures the preview found by guessing into the document, so an edited model keeps them.
fn adopt_guesses(root: &mut Value, images: &mut Vec<DocImage>, model: &Model) {
    let count = root.get("materials").and_then(Value::as_array).map_or(0, Vec::len);
    let mut adopted: HashMap<u32, usize> = HashMap::new();
    for index in 0..count {
        let Some(guess) = model.materials.get(index).and_then(|m| m.image) else { continue };
        let has_one = root["materials"][index].get("pbrMetallicRoughness").and_then(|p| p.get("baseColorTexture")).is_some();
        if has_one { continue }
        let Some(source) = model.images.get(guess as usize) else { continue };
        let texture = *adopted.entry(guess).or_insert_with(|| {
            images.push(DocImage::from_pixels(Arc::new(Image { width: source.width, height: source.height, rgba: source.rgba.clone() })));
            push_entry(root, "images", json!({}));
            push_entry(root, "textures", json!({ "source": images.len() - 1 }))
        });
        let material = &mut root["materials"][index];
        if material.get("pbrMetallicRoughness").is_none() { material["pbrMetallicRoughness"] = json!({}); }
        material["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": texture });
        material["pbrMetallicRoughness"]["baseColorFactor"] = json!([1.0, 1.0, 1.0, 1.0]);
    }
}

/// Appends to a top-level glTF array, returning the new entry's index.
fn push_entry(root: &mut Value, key: &str, value: Value) -> usize {
    if root.get(key).is_none_or(|v| !v.is_array()) { root[key] = json!([]); }
    let list = root[key].as_array_mut().expect("just made an array");
    list.push(value);
    list.len() - 1
}

/// Builds a document from a mesh that came from somewhere glTF can't be patched.
fn from_flat(mut model: Model) -> ModelDoc {
    let mut root = json!({ "asset": { "version": "2.0", "generator": "Asset Browser" }, "scene": 0, "scenes": [{ "nodes": [0] }], "nodes": [{ "mesh": 0 }] });
    let mut buffer: Vec<u8> = Vec::new();
    let mut view = |root: &mut Value, data: &[u8], target: u32| {
        buffer.resize(buffer.len().next_multiple_of(4), 0);
        let index = push_entry(root, "bufferViews", json!({ "buffer": 0, "byteOffset": buffer.len(), "byteLength": data.len(), "target": target }));
        buffer.extend_from_slice(data);
        index
    };

    let (mut low, mut high) = ([f32::MAX; 3], [f32::MIN; 3]);
    for vertex in &model.vertices {
        for axis in 0..3 {
            low[axis] = low[axis].min(vertex.pos[axis]);
            high[axis] = high[axis].max(vertex.pos[axis]);
        }
    }
    let positions: Vec<[f32; 3]> = model.vertices.iter().map(|v| v.pos).collect();
    let normals: Vec<[f32; 3]> = model.vertices.iter().map(|v| if v.normal == [0.0; 3] { [0.0, 1.0, 0.0] } else { v.normal }).collect();
    let uvs: Vec<[f32; 2]> = model.vertices.iter().map(|v| v.uv).collect();
    let count = model.vertices.len();
    let position_view = view(&mut root, bytemuck::cast_slice(&positions), 34962);
    let normal_view = view(&mut root, bytemuck::cast_slice(&normals), 34962);
    let uv_view = view(&mut root, bytemuck::cast_slice(&uvs), 34962);
    let index_view = view(&mut root, bytemuck::cast_slice(&model.indices), 34963);
    let position = push_entry(&mut root, "accessors", json!({ "bufferView": position_view, "componentType": 5126, "count": count, "type": "VEC3", "min": low, "max": high }));
    let normal = push_entry(&mut root, "accessors", json!({ "bufferView": normal_view, "componentType": 5126, "count": count, "type": "VEC3" }));
    let uv = push_entry(&mut root, "accessors", json!({ "bufferView": uv_view, "componentType": 5126, "count": count, "type": "VEC2" }));

    let mut images = Vec::new();
    // Each image's texture, and whether it has cut-out pixels: its pixels move out on first use.
    let mut image_textures: HashMap<u32, (usize, bool)> = HashMap::new();
    let mut flat = vec![None; model.materials.len()];
    // COLLADA gives every primitive its own copy of a material, and those are one material here.
    let mut same: HashMap<(String, Option<u32>, [u32; 4]), usize> = HashMap::new();
    let mut primitives = Vec::new();
    for part in model.parts.clone() {
        let id = part.material as usize;
        let material = model.materials[id];
        let name = model.sources.get(id).map_or_else(String::new, |s| s.name.clone());
        let key = (name.clone(), material.image, material.color.map(f32::to_bits));
        let index = *same.entry(key).or_insert_with(|| {
            let mut pbr = json!({ "baseColorFactor": material.color, "roughnessFactor": 1.0, "metallicFactor": 0.0 });
            let mut entry = json!({ "doubleSided": true });
            if let Some(found) = material.image.and_then(|i| model.images.get_mut(i as usize).map(|image| (i, image))) {
                let (image_index, image) = found;
                let (texture, cutout) = *image_textures.entry(image_index).or_insert_with(|| {
                    let cutout = image.rgba.chunks_exact(4).any(|p| p[3] < 128);
                    let pixels = Image { width: image.width, height: image.height, rgba: std::mem::take(&mut image.rgba) };
                    images.push(DocImage::from_pixels(Arc::new(pixels)));
                    push_entry(&mut root, "images", json!({}));
                    (push_entry(&mut root, "textures", json!({ "source": images.len() - 1 })), cutout)
                });
                pbr["baseColorTexture"] = json!({ "index": texture });
                if cutout { entry["alphaMode"] = json!("MASK"); }
            }
            entry["pbrMetallicRoughness"] = pbr;
            if !name.is_empty() { entry["name"] = json!(name); }
            push_entry(&mut root, "materials", entry)
        });
        flat[id] = Some(index);
        let accessor = push_entry(&mut root, "accessors", json!({ "bufferView": index_view, "byteOffset": part.start as usize * 4, "componentType": 5125, "count": part.count, "type": "SCALAR" }));
        primitives.push(json!({ "attributes": { "POSITION": position, "NORMAL": normal, "TEXCOORD_0": uv }, "indices": accessor, "material": index }));
    }
    root["meshes"] = json!([{ "primitives": primitives }]);
    let materials = read_materials(&root);
    let image_filters = vec![None; images.len()];
    ModelDoc { root, buffer, images, materials, flat, image_filters, rebuilt: Mutex::default(), keeps_scene: false, bounds: ([0.0; 3], [0.0; 3]) }
}

/// Two 4-byte chunks' worth of GLB around `json` and `bin`.
fn pack_glb(json: &[u8], bin: &[u8]) -> Vec<u8> {
    let json_padded = json.len().next_multiple_of(4);
    let bin_padded = bin.len().next_multiple_of(4);
    let bin_chunk = if bin.is_empty() { 0 } else { 8 + bin_padded };
    let total = 12 + 8 + json_padded + bin_chunk;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_padded as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(json);
    out.resize(12 + 8 + json_padded, b' ');
    if !bin.is_empty() {
        out.extend_from_slice(&(bin_padded as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(bin);
        out.resize(total, 0);
    }
    out
}

impl ModelDoc {
    /// What's shown for a material once `edit` is applied: colour, roughness, metallic, emissive.
    pub fn values(&self, material: usize, edit: &ModelEdit) -> ([f32; 4], f32, f32, [f32; 3]) {
        let info = &self.materials[material];
        let change = edit.materials.get(&material).copied().unwrap_or_default();
        let color = change.color.map_or(info.color, |c| [c[0], c[1], c[2], info.color[3]]);
        let emissive = change.emissive.map_or(info.emissive, |target| {
            let peak = info.emissive.iter().copied().fold(0.0, f32::max);
            if peak > 0.0 { info.emissive.map(|c| c / peak * target) } else { [target; 3] }
        });
        (color, change.roughness.unwrap_or(info.roughness), change.metallic.unwrap_or(info.metallic), emissive)
    }

    /// How the normal map is weighted under `edit`.
    pub fn normal_strength(&self, material: usize, edit: &ModelEdit) -> f32 {
        edit.materials.get(&material).and_then(|m| m.normal).unwrap_or(self.materials[material].normal_scale)
    }

    /// Document material `index` as it's drawn under `edit`.
    pub fn material_surface(&self, index: usize, edit: &ModelEdit) -> SurfaceMaterial {
        let (color, roughness, metallic, emissive) = self.values(index, edit);
        let info = &self.materials[index];
        let map = |slot| info.map(slot).map(|i| i as u32);
        let filter = |slot| info.map(slot).and_then(|image| self.filter_of(image, edit));
        SurfaceMaterial {
            filters: [filter(Slot::Base), filter(Slot::MetalRough), filter(Slot::Normal), filter(Slot::Emissive)],
            color,
            roughness,
            metallic,
            emissive,
            image: map(Slot::Base),
            surface_image: map(Slot::MetalRough),
            normal_image: map(Slot::Normal),
            emissive_image: map(Slot::Emissive),
            normal_scale: self.normal_strength(index, edit),
            doc: Some(index as u32),
        }
    }

    /// The preview's materials under `edit`, drawing on `images`.
    pub fn surface(&self, edit: &ModelEdit, images: Vec<Arc<Image>>) -> Surface {
        let default_material = model::Material::default();
        let materials = self.flat.iter().map(|doc| match doc {
            Some(index) => self.material_surface(*index, edit),
            None => SurfaceMaterial { color: default_material.color, roughness: 1.0, metallic: 0.0, emissive: [0.0; 3], image: None, surface_image: None, normal_image: None, emissive_image: None, normal_scale: 1.0, doc: None, filters: [None; 4] },
        }).collect();
        Surface { materials, images, focus: None, origin: None }
    }

    /// Where `origin` puts the model's origin, in the file's coordinates.
    pub fn pivot(&self, origin: Option<Origin>) -> [f32; 3] {
        let (low, high) = self.bounds;
        let middle = |axis: usize| (low[axis] + high[axis]) * 0.5;
        match origin {
            None => [0.0; 3],
            Some(Origin::Centre) => [middle(0), middle(1), middle(2)],
            Some(Origin::Base) => [middle(0), low[1], middle(2)],
        }
    }

    /// The model's width, height and depth once scaled.
    pub fn scaled_size(&self, scale: Option<f32>) -> [f32; 3] {
        let (low, high) = self.bounds;
        let scale = scale.unwrap_or(1.0);
        [(high[0] - low[0]) * scale, (high[1] - low[1]) * scale, (high[2] - low[2]) * scale]
    }

    /// Puts a new root node over every scene, moving and scaling everything under it, so nodes,
    /// skeletons and animation keep working.
    fn apply_transform(&self, root: &mut Value, edit: &ModelEdit) {
        if edit.origin.is_none() && edit.scale.is_none() { return }
        let scale = edit.scale.unwrap_or(1.0);
        let pivot = self.pivot(edit.origin);
        let moved = [-pivot[0] * scale, -pivot[1] * scale, -pivot[2] * scale];
        for index in 0..root.get("scenes").and_then(Value::as_array).map_or(0, Vec::len) {
            let Some(children) = root["scenes"][index].get("nodes").cloned().filter(|n| n.as_array().is_some_and(|a| !a.is_empty())) else { continue };
            let parent = push_entry(root, "nodes", json!({ "name": "Root", "translation": moved, "scale": [scale, scale, scale], "children": children }));
            root["scenes"][index]["nodes"] = json!([parent]);
        }
    }

    /// What `image` is sampled with under `edit`: its own setting, then the whole model's, then
    /// the file's.
    pub fn filter_of(&self, image: usize, edit: &ModelEdit) -> Option<TextureFilter> {
        edit.textures.get(&image).copied().or(edit.filter).or_else(|| self.file_filter(image))
    }

    /// What the file asks `image` to be sampled with, if it says.
    pub fn file_filter(&self, image: usize) -> Option<TextureFilter> {
        self.image_filters.get(image).copied().flatten()
    }

    /// Writes filters into samplers. Each change gets a sampler of its own, keeping the original's
    /// wrap modes, because the one a texture points at may be shared with others.
    fn apply_filters(root: &mut Value, edit: &ModelEdit) {
        if edit.filter.is_none() && edit.textures.is_empty() { return }
        for index in 0..root.get("textures").and_then(Value::as_array).map_or(0, Vec::len) {
            let Some(image) = root["textures"][index].get("source").and_then(Value::as_u64) else { continue };
            let Some(filter) = edit.textures.get(&(image as usize)).copied().or(edit.filter) else { continue };
            let current = root["textures"][index].get("sampler").and_then(Value::as_u64).and_then(|s| root.get("samplers")?.get(s as usize)).cloned();
            let mut sampler = current.unwrap_or_else(|| json!({}));
            let (mag, min) = filter.codes();
            sampler["magFilter"] = json!(mag);
            sampler["minFilter"] = json!(min);
            let known = root.get("samplers").and_then(Value::as_array).and_then(|all| all.iter().position(|s| *s == sampler));
            let at = known.unwrap_or_else(|| push_entry(root, "samplers", sampler));
            root["textures"][index]["sampler"] = json!(at);
        }
    }

    /// How many different images the materials use.
    pub fn texture_count(&self) -> usize {
        self.materials.iter().flat_map(|m| m.maps.iter().map(|(_, i)| *i)).collect::<std::collections::HashSet<_>>().len()
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// The source's own images, as the preview needs them.
    pub fn original_images(&self) -> Vec<Arc<Image>> {
        self.images.iter().map(|image| image.pixels.get().cloned().flatten().unwrap_or_else(blank)).collect()
    }

    pub fn image_size(&self, image: usize) -> Option<(u32, u32)> {
        self.images.get(image).and_then(|i| i.size)
    }

    /// The longest side among the images `material` uses, if any can be resized.
    pub fn longest_texture(&self, material: usize) -> Option<u32> {
        self.materials[material].maps.iter().filter(|(_, i)| self.images[*i].resizable()).filter_map(|(_, i)| self.image_size(*i)).map(|(w, h)| w.max(h)).max()
    }

    /// The side `image` should leave at. An image shared with a material that keeps it full size
    /// stays full size.
    fn image_limit(&self, image: usize, edit: &ModelEdit) -> Option<u32> {
        let longest = self.image_size(image).map(|(w, h)| w.max(h))?;
        if !self.images[image].resizable() { return None }
        let mut limit: Option<u32> = None;
        let users = self.materials.iter().enumerate().filter(|(_, m)| m.maps.iter().any(|(_, i)| *i == image)).map(|(index, _)| index);
        for index in users {
            let asked = edit.materials.get(&index).and_then(|m| m.max_texture)?;
            limit = Some(limit.map_or(asked, |l| l.max(asked)));
        }
        limit.filter(|l| *l < longest)
    }

    /// `image` at `max` (0 for its own size), encoded. Results are kept for the session.
    fn rebuild(&self, image: usize, max: u32, nearest: bool, flip_green: bool) -> Result<Arc<Rebuilt>, String> {
        let key = (image, max, nearest, flip_green);
        if let Some(done) = self.rebuilt.lock().map_err(|e| e.to_string())?.get(&key) { return Ok(done.clone()) }
        let source = self.images[image].pixels().ok_or("a texture couldn't be decoded")?;
        let pixels = if max == 0 {
            source
        } else {
            let buffer = image::RgbaImage::from_raw(source.width, source.height, source.rgba.clone()).ok_or("a texture has the wrong size")?;
            let scaled = crate::export::scale(&image::DynamicImage::ImageRgba8(buffer), max, nearest).into_rgba8();
            Arc::new(Image { width: scaled.width(), height: scaled.height(), rgba: scaled.into_raw() })
        };
        let pixels = if flip_green {
            let mut rgba = pixels.rgba.clone();
            picture::invert_green(&mut rgba);
            Arc::new(Image { width: pixels.width, height: pixels.height, rgba })
        } else {
            pixels
        };
        let buffer = image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.rgba.clone()).ok_or("a texture has the wrong size")?;
        let (bytes, mime) = if self.images[image].is_jpeg() {
            let mut out = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            image::DynamicImage::ImageRgba8(buffer).to_rgb8().write_with_encoder(encoder).map_err(|e| e.to_string())?;
            (out, "image/jpeg")
        } else {
            (picture::encode(&buffer, ImageEdit::default())?.0, "image/png")
        };
        let done = Arc::new(Rebuilt { pixels, bytes, mime });
        self.rebuilt.lock().map_err(|e| e.to_string())?.insert(key, done.clone());
        Ok(done)
    }

    fn apply_values(&self, root: &mut Value, edit: &ModelEdit) {
        for (index, change) in &edit.materials {
            let Some(material) = root.get_mut("materials").and_then(|m| m.get_mut(*index)) else { continue };
            let (color, roughness, metallic, emissive) = self.values(*index, edit);
            if material.get("pbrMetallicRoughness").is_none() { material["pbrMetallicRoughness"] = json!({}); }
            let pbr = &mut material["pbrMetallicRoughness"];
            if change.color.is_some() { pbr["baseColorFactor"] = json!(color); }
            if change.roughness.is_some() { pbr["roughnessFactor"] = json!(roughness); }
            if change.metallic.is_some() { pbr["metallicFactor"] = json!(metallic); }
            if change.emissive.is_some() { material["emissiveFactor"] = json!(emissive); }
            if let Some(scale) = change.normal && material.get("normalTexture").is_some() { material["normalTexture"]["scale"] = json!(scale); }
        }
    }

    /// The GLB under `edit`, and the images it holds for the preview.
    pub fn render(&self, edit: &ModelEdit, nearest: bool) -> Result<(Vec<u8>, Vec<Arc<Image>>), String> {
        let mut root = self.root.clone();
        self.apply_values(&mut root, edit);
        Self::apply_filters(&mut root, edit);
        self.apply_transform(&mut root, edit);
        let mut bin: Vec<u8> = Vec::with_capacity(self.buffer.len());

        // Geometry first. A view keeps its alignment, because accessors inside it rely on that.
        let image_views: Vec<usize> = self.images.iter().filter_map(|i| i.view).collect();
        if let Some(views) = root.get_mut("bufferViews").and_then(Value::as_array_mut) {
            for (index, view) in views.iter_mut().enumerate().filter(|(i, _)| !image_views.contains(i)) {
                let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
                let length = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
                let data = self.buffer.get(offset..offset + length).ok_or_else(|| format!("buffer view {index} runs past the data"))?;
                while bin.len() % 8 != offset % 8 { bin.push(0); }
                view["byteOffset"] = json!(bin.len());
                bin.extend_from_slice(data);
            }
        }

        let mut shown = Vec::with_capacity(self.images.len());
        for (index, image) in self.images.iter().enumerate() {
            let limit = self.image_limit(index, edit);
            let flip = edit.flip_normal.contains(&index) && image.resizable();
            let rebuilt = match (limit, &image.encoded) {
                (Some(max), _) => Some(self.rebuild(index, max, nearest, flip)?),
                (None, None) if image.size.is_some() => Some(self.rebuild(index, 0, nearest, flip)?),
                (None, _) if flip => Some(self.rebuild(index, 0, nearest, true)?),
                _ => None,
            };
            let (bytes, mime): (&[u8], &str) = match (&rebuilt, &image.encoded) {
                (Some(done), _) => (&done.bytes, done.mime),
                (None, Some((bytes, mime))) => (bytes, mime),
                (None, None) => {
                    shown.push(blank());
                    continue;
                }
            };
            shown.push(rebuilt.as_ref().map_or_else(|| image.pixels.get().cloned().flatten().unwrap_or_else(blank), |done| done.pixels.clone()));
            while !bin.len().is_multiple_of(4) { bin.push(0); }
            let view = json!({ "buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len() });
            bin.extend_from_slice(bytes);
            let at = match image.view {
                Some(at) => {
                    root["bufferViews"][at] = view;
                    at
                }
                None => push_entry(&mut root, "bufferViews", view),
            };
            let entry = &mut root["images"][index];
            entry["bufferView"] = json!(at);
            entry["mimeType"] = json!(mime);
            if let Some(object) = entry.as_object_mut() { object.remove("uri"); }
        }

        if bin.is_empty() {
            if let Some(object) = root.as_object_mut() { object.remove("buffers"); }
        } else {
            root["buffers"] = json!([{ "byteLength": bin.len() }]);
        }
        let json = serde_json::to_vec(&root).map_err(|e| e.to_string())?;
        Ok((pack_glb(&json, &bin), shown))
    }
}

/// Stands in for an image the preview never samples.
fn blank() -> Arc<Image> {
    static WHITE: OnceLock<Arc<Image>> = OnceLock::new();
    WHITE.get_or_init(|| Arc::new(Image { width: 1, height: 1, rgba: vec![255; 4] })).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let dir = std::env::temp_dir().join(format!("ab-mesh-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    /// A triangle with a 4×2 texture, as OBJ, opened into a document.
    fn textured_obj(dir: &Dir) -> ModelDoc {
        image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 40, 40, 255])).save(dir.0.join("t.png")).unwrap();
        std::fs::write(dir.0.join("m.mtl"), "newmtl shiny\nmap_Kd t.png\n").unwrap();
        std::fs::write(dir.0.join("a.obj"), "mtllib m.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nvn 0 0 1\nusemtl shiny\nf 1/1/1 2/2/1 3/3/1\n").unwrap();
        open(&dir.0.join("a.obj"), &dir.0).unwrap()
    }

    fn resize_all(max: u32) -> ModelEdit {
        ModelEdit::single(0, MaterialEdit { max_texture: Some(max), ..MaterialEdit::default() })
    }

    #[test]
    fn a_flat_model_writes_a_glb_that_loads_with_its_material_and_texture() {
        let dir = Dir::new("flat");
        let doc = textured_obj(&dir);
        assert!(!doc.keeps_scene);
        assert_eq!(doc.materials.len(), 1);
        assert_eq!(doc.materials[0].name, "shiny");
        assert_eq!(doc.materials[0].map(Slot::Base), Some(0));

        let edit = ModelEdit::single(0, MaterialEdit { roughness: Some(0.25), ..MaterialEdit::default() });
        let (bytes, _) = doc.render(&edit, false).unwrap();
        let out = dir.0.join("out.glb");
        std::fs::write(&out, &bytes).unwrap();

        let model = model::load(&out, &dir.0).unwrap();
        assert_eq!((model.triangles(), model.images.len()), (1, 1));
        assert_eq!((model.images[0].width, model.images[0].height), (4, 2));
        let reopened = open(&out, &dir.0).unwrap();
        assert_eq!(reopened.materials[0].roughness, 0.25);
        assert!(reopened.keeps_scene);
    }

    #[test]
    fn resizing_changes_the_stored_texture_and_is_reused() {
        let dir = Dir::new("resize");
        let doc = textured_obj(&dir);
        let (bytes, shown) = doc.render(&resize_all(2), false).unwrap();
        assert_eq!((shown[0].width, shown[0].height), (2, 1));
        let out = dir.0.join("out.glb");
        std::fs::write(&out, &bytes).unwrap();
        assert_eq!(model::load(&out, &dir.0).unwrap().images[0].width, 2);
        doc.render(&resize_all(2), false).unwrap();
        assert_eq!(doc.rebuilt.lock().unwrap().len(), 1, "the texture was only made once");
    }

    #[test]
    fn a_glb_keeps_everything_that_is_not_a_material() {
        let dir = Dir::new("keep");
        let doc = textured_obj(&dir);
        let mut root = doc.root.clone();
        root["extras"] = json!({ "kept": true });
        root["nodes"][0]["name"] = json!("Hips");
        doc.images[0].pixels();
        let (first, _) = doc.render(&ModelEdit::default(), false).unwrap();
        let glb = gltf::binary::Glb::from_slice(&first).unwrap();
        let mut json: Value = serde_json::from_slice(&glb.json).unwrap();
        json["extras"] = root["extras"].clone();
        json["nodes"] = root["nodes"].clone();
        let source = dir.0.join("rig.glb");
        std::fs::write(&source, pack_glb(&serde_json::to_vec(&json).unwrap(), &glb.bin.clone().unwrap())).unwrap();

        let source_doc = open(&source, &dir.0).unwrap();
        let edit = ModelEdit::single(0, MaterialEdit { metallic: Some(0.5), ..MaterialEdit::default() });
        let (bytes, _) = source_doc.render(&edit, false).unwrap();
        let out = gltf::binary::Glb::from_slice(&bytes).unwrap();
        let written: Value = serde_json::from_slice(&out.json).unwrap();
        assert_eq!(written["extras"]["kept"], json!(true));
        assert_eq!(written["nodes"][0]["name"], json!("Hips"));
        assert_eq!(written["materials"][0]["pbrMetallicRoughness"]["metallicFactor"], json!(0.5));
        let (original_image, written_image) = (&source_doc.images[0].encoded.as_ref().unwrap().0, {
            let view = &written["bufferViews"][written["images"][0]["bufferView"].as_u64().unwrap() as usize];
            let (o, l) = (view["byteOffset"].as_u64().unwrap() as usize, view["byteLength"].as_u64().unwrap() as usize);
            out.bin.as_ref().unwrap()[o..o + l].to_vec()
        });
        assert_eq!(original_image, &written_image, "an image nobody resized leaves byte for byte");
    }

    #[test]
    fn a_glb_written_twice_is_identical_and_the_geometry_survives() {
        let dir = Dir::new("twice");
        let doc = textured_obj(&dir);
        let (a, _) = doc.render(&resize_all(2), false).unwrap();
        let (b, _) = doc.render(&resize_all(2), false).unwrap();
        assert_eq!(a, b);
        let out = dir.0.join("out.glb");
        std::fs::write(&out, &a).unwrap();
        let model = model::load(&out, &dir.0).unwrap();
        assert_eq!(model.vertices.iter().filter(|v| v.pos == [1.0, 0.0, 0.0]).count(), 1);
    }

    #[test]
    fn a_shared_image_stays_full_size_unless_every_user_shrinks_it() {
        let dir = Dir::new("shared");
        let mut doc = textured_obj(&dir);
        let mut second = doc.materials[0].clone();
        second.name = "other".into();
        doc.materials.push(second);
        assert_eq!(doc.image_limit(0, &resize_all(2)), None, "the second material still wants it whole");
        let both = ModelEdit { materials: [(0, MaterialEdit { max_texture: Some(2), ..MaterialEdit::default() }), (1, MaterialEdit { max_texture: Some(3), ..MaterialEdit::default() })].into(), ..ModelEdit::default() };
        assert_eq!(doc.image_limit(0, &both), Some(3), "the larger request wins");
    }

    /// Two textures sharing a clamped sampler, one of which also uses the second image.
    fn sampled_glb(dir: &Dir) -> ModelDoc {
        let doc = textured_obj(dir);
        let (flat, _) = doc.render(&ModelEdit::default(), false).unwrap();
        let glb = gltf::binary::Glb::from_slice(&flat).unwrap();
        let mut json: Value = serde_json::from_slice(&glb.json).unwrap();
        json["samplers"] = json!([{ "magFilter": 9729, "wrapS": 33071, "wrapT": 33071 }]);
        json["textures"] = json!([{ "source": 0, "sampler": 0 }, { "source": 0, "sampler": 0 }]);
        let source = dir.0.join("sampled.glb");
        std::fs::write(&source, pack_glb(&serde_json::to_vec(&json).unwrap(), &glb.bin.clone().unwrap())).unwrap();
        open(&source, &dir.0).unwrap()
    }

    fn written(doc: &ModelDoc, edit: &ModelEdit) -> Value {
        let (bytes, _) = doc.render(edit, false).unwrap();
        serde_json::from_slice(&gltf::binary::Glb::from_slice(&bytes).unwrap().json).unwrap()
    }

    #[test]
    fn a_filter_is_written_to_a_new_sampler_that_keeps_the_wrap_modes() {
        let dir = Dir::new("sampler");
        let doc = sampled_glb(&dir);
        assert_eq!(doc.file_filter(0), Some(TextureFilter::Smooth));
        let json = written(&doc, &ModelEdit { filter: Some(TextureFilter::Pixels), ..ModelEdit::default() });
        let at = json["textures"][0]["sampler"].as_u64().unwrap() as usize;
        assert_eq!(json["samplers"][at], json!({ "magFilter": 9728, "minFilter": 9984, "wrapS": 33071, "wrapT": 33071 }));
        assert_eq!(json["samplers"][0]["magFilter"], json!(9729), "the file's own sampler is left alone");
        assert_eq!(json["samplers"].as_array().unwrap().len(), 2, "both textures share the one new sampler");
    }

    #[test]
    fn a_textures_own_filter_beats_the_models_and_the_files_is_the_fallback() {
        let dir = Dir::new("precedence");
        let doc = sampled_glb(&dir);
        assert_eq!(doc.filter_of(0, &ModelEdit::default()), Some(TextureFilter::Smooth));
        let whole = ModelEdit { filter: Some(TextureFilter::Pixels), ..ModelEdit::default() };
        assert_eq!(doc.filter_of(0, &whole), Some(TextureFilter::Pixels));
        let own = ModelEdit { textures: [(0, TextureFilter::Smooth)].into(), ..whole };
        assert_eq!(doc.filter_of(0, &own), Some(TextureFilter::Smooth));
        assert!(!own.is_unchanged());
        assert_eq!(doc.material_surface(0, &own).filters[0], Some(TextureFilter::Smooth));
    }

    fn exported_bounds(doc: &ModelDoc, dir: &Dir, edit: &ModelEdit) -> ([f32; 3], [f32; 3]) {
        let (bytes, _) = doc.render(edit, false).unwrap();
        let out = dir.0.join("moved.glb");
        std::fs::write(&out, &bytes).unwrap();
        open(&out, &dir.0).unwrap().bounds
    }

    #[test]
    fn origin_and_scale_move_the_whole_model_under_a_new_root() {
        let dir = Dir::new("origin");
        let doc = textured_obj(&dir);
        assert_eq!(doc.bounds, ([0.0, 0.0, 0.0], [1.0, 1.0, 0.0]));

        let centre = exported_bounds(&doc, &dir, &ModelEdit { origin: Some(Origin::Centre), ..ModelEdit::default() });
        assert_eq!(centre, ([-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]));
        let base = exported_bounds(&doc, &dir, &ModelEdit { origin: Some(Origin::Base), ..ModelEdit::default() });
        assert_eq!(base, ([-0.5, 0.0, 0.0], [0.5, 1.0, 0.0]));
        // The origin stays put at the model's base however large it's made.
        let bigger = exported_bounds(&doc, &dir, &ModelEdit { origin: Some(Origin::Base), scale: Some(100.0), ..ModelEdit::default() });
        assert_eq!(bigger, ([-50.0, 0.0, 0.0], [50.0, 100.0, 0.0]));
        assert_eq!(doc.scaled_size(Some(100.0)), [100.0, 100.0, 0.0]);
    }

    #[test]
    fn the_new_root_keeps_the_old_nodes_beneath_it() {
        let dir = Dir::new("rootnode");
        let doc = textured_obj(&dir);
        let json = written(&doc, &ModelEdit { scale: Some(2.0), ..ModelEdit::default() });
        let top = json["scenes"][0]["nodes"][0].as_u64().unwrap() as usize;
        assert_eq!(json["nodes"][top]["children"], json!([0]));
        assert_eq!(json["nodes"][top]["scale"], json!([2.0, 2.0, 2.0]));
        assert!(!ModelEdit { scale: Some(2.0), ..ModelEdit::default() }.is_unchanged());
    }

    #[test]
    fn flipping_a_normal_map_rewrites_only_its_green() {
        let dir = Dir::new("flipnormal");
        let doc = textured_obj(&dir);
        let edit = ModelEdit { flip_normal: [0].into(), ..ModelEdit::default() };
        let (bytes, shown) = doc.render(&edit, false).unwrap();
        assert_eq!(&shown[0].rgba[..4], &[200, 215, 40, 255], "was 200, 40, 40");
        let out = dir.0.join("flip.glb");
        std::fs::write(&out, &bytes).unwrap();
        assert_eq!(&model::load(&out, &dir.0).unwrap().images[0].rgba[..4], &[200, 215, 40, 255]);
    }

    #[test]
    fn a_flat_model_with_no_samplers_gets_one_when_asked() {
        let dir = Dir::new("flatfilter");
        let doc = textured_obj(&dir);
        assert_eq!(doc.file_filter(0), None);
        let json = written(&doc, &ModelEdit { textures: [(0, TextureFilter::Pixels)].into(), ..ModelEdit::default() });
        assert_eq!(json["samplers"][0]["magFilter"], json!(9728));
        assert_eq!(json["textures"][0]["sampler"], json!(0));
    }

    #[test]
    fn untouched_edits_count_as_no_edit() {
        let mut edit = ModelEdit::default();
        assert!(edit.is_unchanged());
        edit.materials.insert(3, MaterialEdit::default());
        assert!(edit.is_unchanged());
        edit.materials.insert(4, MaterialEdit { roughness: Some(0.1), ..MaterialEdit::default() });
        assert!(!edit.is_unchanged());
    }
}
