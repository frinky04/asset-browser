//! Material edits, and writing models out as GLB. A glTF source keeps its whole document (nodes,
//! skins, animation, extensions) and only has its materials and images changed; anything else
//! becomes a static document built from the mesh the preview already has. Both then go through
//! the same edit and write steps.

use std::collections::{BTreeMap, HashMap};
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
    /// The largest side this material's textures leave at.
    pub max_texture: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelEdit {
    pub materials: BTreeMap<usize, MaterialEdit>,
}

impl ModelEdit {
    pub fn is_unchanged(&self) -> bool {
        self.materials.values().all(|m| *m == MaterialEdit::default())
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
}

/// One material, as the file has it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialInfo {
    pub name: String,
    pub color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub emissive: [f32; 3],
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
}

/// What the preview draws a model's materials with.
#[derive(Clone)]
pub struct Surface {
    pub materials: Vec<SurfaceMaterial>,
    pub images: Vec<Arc<Image>>,
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
    /// Slow to make, and a material slider shouldn't redo them. Bounded by the sizes on offer.
    rebuilt: Mutex<HashMap<(usize, u32, bool), Arc<Rebuilt>>>,
    /// Whether this is a whole glTF document, rather than a flattened mesh.
    pub keeps_scene: bool,
}

pub fn open(path: &Path, library: &Path) -> Result<ModelDoc, String> {
    let model = model::load(path, library)?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let doc = if matches!(ext.as_str(), "glb" | "gltf") { from_gltf(path, model)? } else { from_flat(model) };
    // Decoded now, off the UI thread, because the preview's shading uses them straight away.
    for material in &doc.materials {
        for slot in [Slot::Base, Slot::MetalRough] {
            if let Some(image) = material.map(slot) { doc.images[image].pixels(); }
        }
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
    Ok(ModelDoc { root, buffer, images, materials, flat, rebuilt: Mutex::default(), keeps_scene: true })
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
    ModelDoc { root, buffer, images, materials, flat, rebuilt: Mutex::default(), keeps_scene: false }
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

    /// The preview's materials under `edit`, drawing on `images`.
    pub fn surface(&self, edit: &ModelEdit, images: Vec<Arc<Image>>) -> Surface {
        let default_material = model::Material::default();
        let materials = self.flat.iter().map(|doc| match doc {
            Some(index) => {
                let (color, roughness, metallic, emissive) = self.values(*index, edit);
                let info = &self.materials[*index];
                SurfaceMaterial { color, roughness, metallic, emissive, image: info.map(Slot::Base).map(|i| i as u32), surface_image: info.map(Slot::MetalRough).map(|i| i as u32) }
            }
            None => SurfaceMaterial { color: default_material.color, roughness: 1.0, metallic: 0.0, emissive: [0.0; 3], image: None, surface_image: None },
        }).collect();
        Surface { materials, images }
    }

    /// The source's own images, as the preview needs them.
    pub fn original_images(&self) -> Vec<Arc<Image>> {
        self.images.iter().map(|image| image.pixels.get().cloned().flatten().unwrap_or_else(blank)).collect()
    }

    pub fn image_size(&self, image: usize) -> Option<(u32, u32)> {
        self.images.get(image).and_then(|i| i.size)
    }

    /// What a material looks like at a glance: its colour times its base texture's average.
    pub fn swatch(&self, material: usize, edit: &ModelEdit) -> [f32; 3] {
        let (color, ..) = self.values(material, edit);
        let average = self.materials[material].map(Slot::Base).and_then(|i| self.images[i].pixels.get().cloned().flatten()).map_or([1.0; 3], |image| {
            // A few thousand samples is plenty for a 14-point square.
            let every = (image.rgba.len() / 4 / 4096).max(1);
            let (mut sum, mut count) = ([0u64; 3], 0u64);
            for pixel in image.rgba.chunks_exact(4).step_by(every) {
                for channel in 0..3 { sum[channel] += u64::from(pixel[channel]); }
                count += 1;
            }
            sum.map(|s| s as f32 / count.max(1) as f32 / 255.0)
        });
        [color[0] * average[0], color[1] * average[1], color[2] * average[2]]
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
    fn rebuild(&self, image: usize, max: u32, nearest: bool) -> Result<Arc<Rebuilt>, String> {
        let key = (image, max, nearest);
        if let Some(done) = self.rebuilt.lock().map_err(|e| e.to_string())?.get(&key) { return Ok(done.clone()) }
        let source = self.images[image].pixels().ok_or("a texture couldn't be decoded")?;
        let pixels = if max == 0 {
            source
        } else {
            let buffer = image::RgbaImage::from_raw(source.width, source.height, source.rgba.clone()).ok_or("a texture has the wrong size")?;
            let scaled = crate::export::scale(&image::DynamicImage::ImageRgba8(buffer), max, nearest).into_rgba8();
            Arc::new(Image { width: scaled.width(), height: scaled.height(), rgba: scaled.into_raw() })
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
        }
    }

    /// The GLB under `edit`, and the images it holds for the preview.
    pub fn render(&self, edit: &ModelEdit, nearest: bool) -> Result<(Vec<u8>, Vec<Arc<Image>>), String> {
        let mut root = self.root.clone();
        self.apply_values(&mut root, edit);
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
            let rebuilt = match (limit, &image.encoded) {
                (Some(max), _) => Some(self.rebuild(index, max, nearest)?),
                (None, None) if image.size.is_some() => Some(self.rebuild(index, 0, nearest)?),
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
        ModelEdit { materials: [(0, MaterialEdit { max_texture: Some(max), ..MaterialEdit::default() })].into() }
    }

    #[test]
    fn a_flat_model_writes_a_glb_that_loads_with_its_material_and_texture() {
        let dir = Dir::new("flat");
        let doc = textured_obj(&dir);
        assert!(!doc.keeps_scene);
        assert_eq!(doc.materials.len(), 1);
        assert_eq!(doc.materials[0].name, "shiny");
        assert_eq!(doc.materials[0].map(Slot::Base), Some(0));

        let edit = ModelEdit { materials: [(0, MaterialEdit { roughness: Some(0.25), ..MaterialEdit::default() })].into() };
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
        let edit = ModelEdit { materials: [(0, MaterialEdit { metallic: Some(0.5), ..MaterialEdit::default() })].into() };
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
        let both = ModelEdit { materials: [(0, MaterialEdit { max_texture: Some(2), ..MaterialEdit::default() }), (1, MaterialEdit { max_texture: Some(3), ..MaterialEdit::default() })].into() };
        assert_eq!(doc.image_limit(0, &both), Some(3), "the larger request wins");
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
