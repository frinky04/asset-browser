//! Turns an asset into files that are safe to hand to another program. Drags always offer a
//! copy staged here, so nothing a target does can reach the file in the library.

use std::path::{Path, PathBuf};

use crate::library::{Asset, FNV_SEED, Kind, fnv};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Options {
    /// The largest side a texture leaves at; `None` keeps its size.
    pub max_size: Option<u32>,
    /// Downscale by dropping pixels rather than filtering, for pixel art.
    pub nearest: bool,
    /// Bring a model's material and texture files along.
    pub companions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_size: None, nearest: false, companions: true }
    }
}

/// Where the cache, index and staged exports live. `ASSET_BROWSER_DATA` moves them.
pub fn app_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ASSET_BROWSER_DATA") { return dir.into(); }
    std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from).join("AssetBrowserV2")
}

fn stage_root() -> PathBuf {
    app_dir().join("drag")
}

fn stage_id(asset: &Asset, options: Options) -> String {
    let flags = [u8::from(options.nearest), u8::from(options.companions)];
    let h = fnv(fnv(FNV_SEED, &asset.key.to_le_bytes()), &options.max_size.unwrap_or(0).to_le_bytes());
    format!("{:016x}", fnv(h, &flags))
}

/// Stages `asset`, returning the files to hand over: the asset first. Repeating a drag reuses
/// the copy already made.
pub fn stage(source: &Path, asset: &Asset, options: Options) -> Result<Vec<PathBuf>, String> {
    stage_in(&stage_root(), source, asset, options)
}

fn stage_in(root: &Path, source: &Path, asset: &Asset, options: Options) -> Result<Vec<PathBuf>, String> {
    let dir = root.join(stage_id(asset, options));
    if let Some(files) = existing(&dir) { return Ok(files); }
    let building = root.join(format!("{}.{:?}", stage_id(asset, options), std::thread::current().id()).replace(['(', ')'], ""));
    let _ = std::fs::remove_dir_all(&building);
    std::fs::create_dir_all(&building).map_err(|e| e.to_string())?;
    let result = fill(&building, source, asset, options);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&building);
        return Err(e);
    }
    if std::fs::rename(&building, &dir).is_err() {
        // Another thread staged the same export first.
        let _ = std::fs::remove_dir_all(&building);
    }
    existing(&dir).ok_or_else(|| "the staged copy disappeared".to_owned())
}

/// The files of a finished stage, listed in the order `fill` recorded them.
fn existing(dir: &Path) -> Option<Vec<PathBuf>> {
    let list = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let files: Vec<PathBuf> = list.lines().map(|name| dir.join(name)).collect();
    (!files.is_empty() && files.iter().all(|f| f.is_file())).then_some(files)
}

const MANIFEST: &str = ".files";

fn fill(dir: &Path, source: &Path, asset: &Asset, options: Options) -> Result<(), String> {
    let mut names = Vec::new();
    match asset.kind {
        Kind::Texture => match options.max_size {
            Some(max) => names.push(write_resized(dir, source, asset, max, options.nearest)?),
            None => names.push(copy_beside(dir, source, asset.name())?),
        },
        Kind::Audio => names.push(copy_beside(dir, source, asset.name())?),
        Kind::Model => {
            names.push(copy_beside(dir, source, asset.name())?);
            if options.companions {
                companions(dir, source, asset.name(), asset.ext, &mut names);
            }
        }
    }
    std::fs::write(dir.join(MANIFEST), names.join("\n")).map_err(|e| e.to_string())
}

fn copy_beside(dir: &Path, source: &Path, name: &str) -> Result<String, String> {
    std::fs::copy(source, dir.join(name)).map_err(|e| format!("couldn't copy {name}: {e}"))?;
    Ok(name.to_owned())
}

fn base_name(reference: &str) -> &str {
    reference.rsplit(['/', '\\']).next().unwrap_or(reference)
}

/// `name` with its stem suffixed, so a resized copy can sit beside the original.
pub fn resized_name(name: &str, max: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}_{max}.{ext}"),
        None => format!("{name}_{max}"),
    }
}

fn write_resized(dir: &Path, source: &Path, asset: &Asset, max: u32, nearest: bool) -> Result<String, String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(source).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let format = reader.format();
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let icc = decoder.icc_profile().ok().flatten();
    let img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let name = resized_name(asset.name(), max);
    if img.width().max(img.height()) <= max {
        std::fs::copy(source, dir.join(&name)).map_err(|e| e.to_string())?;
        return Ok(name);
    }
    let filter = if nearest { image::imageops::FilterType::Nearest } else { image::imageops::FilterType::Lanczos3 };
    let out = img.resize(max, max, filter);
    let path = dir.join(&name);
    // A resized file keeps its format, so a material that names `.jpg` still finds one.
    match format {
        Some(image::ImageFormat::Jpeg) => {
            let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 95);
            if let Some(profile) = icc { let _ = image::ImageEncoder::set_icc_profile(&mut encoder, profile); }
            out.to_rgb8().write_with_encoder(encoder).map_err(|e| e.to_string())?;
        }
        Some(f) => out.save_with_format(&path, f).map_err(|e| e.to_string())?,
        None => out.save(&path).map_err(|e| e.to_string())?,
    }
    Ok(name)
}

/// Copies what an OBJ, DAE or FBX refers to, flattened beside it, and points the staged OBJ and
/// DAE at the flattened names. An FBX keeps its paths: programs look beside it for a stale one.
fn companions(dir: &Path, source: &Path, name: &str, ext: &str, names: &mut Vec<String>) {
    let base = source.parent().unwrap_or(Path::new("."));
    let bring = |reference: &str, names: &mut Vec<String>| -> Option<String> {
        let found = crate::model::resolve(base, reference)?;
        let flat = base_name(&found.to_string_lossy()).to_owned();
        if !names.contains(&flat) && copy_beside(dir, &found, &flat).is_ok() { names.push(flat.clone()); }
        Some(flat)
    };
    match ext {
        "obj" => {
            let Ok(text) = std::fs::read_to_string(source) else { return };
            let libraries: Vec<&str> = text.lines().filter_map(|l| l.trim().strip_prefix("mtllib ")).map(str::trim).collect();
            let mut changed = text.clone();
            for lib in libraries {
                let Some(flat) = bring(lib, names) else { continue };
                if flat != lib { changed = changed.replace(lib, &flat); }
                let Ok(mtl) = std::fs::read_to_string(dir.join(&flat)) else { continue };
                let mut out = String::with_capacity(mtl.len());
                for line in mtl.lines() {
                    let rewritten = line.trim_start().split_once(char::is_whitespace).and_then(|(tag, rest)| {
                        if !tag.starts_with("map_") && tag != "bump" && tag != "disp" && tag != "decal" { return None; }
                        let reference = rest.split_ascii_whitespace().last()?;
                        let file = bring(reference, names)?;
                        Some(line.replace(reference, &file))
                    });
                    out.push_str(rewritten.as_deref().unwrap_or(line));
                    out.push('\n');
                }
                let _ = std::fs::write(dir.join(&flat), out);
            }
            if changed != text { let _ = std::fs::write(dir.join(name), changed); }
        }
        "dae" => {
            let Ok(text) = std::fs::read_to_string(source) else { return };
            let Ok(doc) = roxmltree::Document::parse(&text) else { return };
            let mut changed = text.clone();
            for reference in doc.descendants().filter(|n| n.has_tag_name("image")).filter_map(|n| n.descendants().find(|c| c.has_tag_name("init_from"))?.text()) {
                let reference = reference.trim();
                let Some(flat) = bring(reference, names) else { continue };
                if flat != reference { changed = changed.replace(&format!(">{reference}<"), &format!(">{flat}<")); }
            }
            if changed != text { let _ = std::fs::write(dir.join(name), changed); }
        }
        "fbx" => {
            for file in crate::model::fbx_texture_files(source) { bring(&file.to_string_lossy(), names); }
        }
        _ => {}
    }
}

/// Bytes held by staged exports.
pub fn staged_bytes() -> u64 {
    let Ok(dirs) = std::fs::read_dir(stage_root()) else { return 0 };
    dirs.filter_map(Result::ok)
        .filter_map(|d| std::fs::read_dir(d.path()).ok())
        .flat_map(|files| files.filter_map(Result::ok).filter_map(|f| f.metadata().ok()).map(|m| m.len()).collect::<Vec<_>>())
        .sum()
}

/// Deletes every staged export. Programs that already took a copy keep it.
pub fn clear_staged() {
    let _ = std::fs::remove_dir_all(stage_root());
}

/// Drops the oldest staged exports past `MAX_STAGED`, and any left half-built. Files in use by
/// the program they were dropped into are copies, so only recent ones are worth keeping.
pub fn prune() {
    const MAX_STAGED: u64 = 8 * 1024 * 1024 * 1024;
    let Ok(dirs) = std::fs::read_dir(stage_root()) else { return };
    let mut stages: Vec<(std::time::SystemTime, u64, PathBuf)> = dirs.filter_map(Result::ok).filter_map(|d| {
        let path = d.path();
        if path.extension().is_some() {
            let _ = std::fs::remove_dir_all(&path);
            return None;
        }
        let used = d.metadata().ok()?.modified().ok()?;
        let size = std::fs::read_dir(&path).ok()?.filter_map(Result::ok).filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum();
        Some((used, size, path))
    }).collect();
    stages.sort_by_key(|(used, ..)| std::cmp::Reverse(*used));
    let mut total = 0;
    for (_, size, path) in stages {
        total += size;
        if total > MAX_STAGED { let _ = std::fs::remove_dir_all(path); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Entry, Library};

    struct Fixture { dir: PathBuf }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!("ab-export-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("lib/Pack/tex")).unwrap();
            Fixture { dir }
        }
        fn root(&self) -> PathBuf { self.dir.join("lib") }
        fn stage(&self) -> PathBuf { self.dir.join("stage") }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); }
    }

    fn asset(root: &Path, rel: &str) -> Asset {
        let meta = std::fs::metadata(root.join(rel)).unwrap();
        Library::build(root.to_path_buf(), vec![Entry { rel: rel.into(), size: meta.len(), mtime: 1 }]).assets.remove(0)
    }

    fn snapshot(p: &Path) -> (Vec<u8>, std::time::SystemTime) {
        (std::fs::read(p).unwrap(), std::fs::metadata(p).unwrap().modified().unwrap())
    }

    #[test]
    fn a_texture_downscales_to_its_max_side_and_the_source_is_untouched() {
        let f = Fixture::new("resize");
        let src = f.root().join("Pack/brick.png");
        image::RgbaImage::from_pixel(400, 200, image::Rgba([9, 8, 7, 255])).save(&src).unwrap();
        let before = snapshot(&src);
        let a = asset(&f.root(), "Pack/brick.png");
        let files = stage_in(&f.stage(), &src, &a, Options { max_size: Some(100), ..Options::default() }).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "brick_100.png");
        assert_eq!(image::image_dimensions(&files[0]).unwrap(), (100, 50));
        assert_eq!(snapshot(&src), before);
        assert_ne!(files[0], src, "the source is never what's handed over");
    }

    #[test]
    fn a_max_above_the_texture_never_upscales() {
        let f = Fixture::new("noup");
        let src = f.root().join("Pack/small.png");
        image::RgbaImage::new(16, 16).save(&src).unwrap();
        let a = asset(&f.root(), "Pack/small.png");
        let files = stage_in(&f.stage(), &src, &a, Options { max_size: Some(512), ..Options::default() }).unwrap();
        assert_eq!(image::image_dimensions(&files[0]).unwrap(), (16, 16));
    }

    #[test]
    fn an_obj_brings_its_material_and_textures_with_flattened_paths() {
        let f = Fixture::new("obj");
        let root = f.root();
        image::RgbaImage::new(2, 2).save(root.join("Pack/tex/wood.png")).unwrap();
        std::fs::write(root.join("Pack/crate.mtl"), "newmtl m\nmap_Kd tex/wood.png\n").unwrap();
        std::fs::write(root.join("Pack/crate.obj"), "mtllib crate.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl m\nf 1 2 3\n").unwrap();
        let a = asset(&root, "Pack/crate.obj");
        let files = stage_in(&f.stage(), &root.join("Pack/crate.obj"), &a, Options::default()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["crate.obj", "crate.mtl", "wood.png"]);
        assert!(std::fs::read_to_string(&files[1]).unwrap().contains("map_Kd wood.png"), "the staged material is repointed");
        assert!(std::fs::read_to_string(root.join("Pack/crate.mtl")).unwrap().contains("tex/wood.png"), "the library's isn't");
        let model = crate::model::load(&files[0]).unwrap();
        assert_eq!(model.images.len(), 1, "the staged model finds its texture");
    }

    #[test]
    fn an_fbx_brings_its_textures_and_the_staged_copy_finds_them() {
        let f = Fixture::new("fbx");
        let root = f.root();
        image::RgbaImage::new(2, 2).save(root.join("Pack/tex/wood.png")).unwrap();
        std::fs::write(root.join("Pack/quad.fbx"), crate::model::FBX_QUAD).unwrap();
        let a = asset(&root, "Pack/quad.fbx");
        let files = stage_in(&f.stage(), &root.join("Pack/quad.fbx"), &a, Options::default()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["quad.fbx", "wood.png"]);
        assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), crate::model::FBX_QUAD, "the staged FBX is a plain copy");
        assert_eq!(crate::model::load(&files[0]).unwrap().images.len(), 1, "the staged model finds its texture");
    }

    #[test]
    fn staging_twice_reuses_the_copy() {
        let f = Fixture::new("again");
        let src = f.root().join("Pack/a.png");
        image::RgbaImage::new(2, 2).save(&src).unwrap();
        let a = asset(&f.root(), "Pack/a.png");
        let one = stage_in(&f.stage(), &src, &a, Options::default()).unwrap();
        let when = std::fs::metadata(&one[0]).unwrap().modified().unwrap();
        let two = stage_in(&f.stage(), &src, &a, Options::default()).unwrap();
        assert_eq!(one, two);
        assert_eq!(std::fs::metadata(&two[0]).unwrap().modified().unwrap(), when);
    }
}
