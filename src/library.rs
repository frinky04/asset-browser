//! The asset index: walking a root folder, remembering it between runs, and answering searches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Texture,
    Model,
    Audio,
}

impl Kind {
    pub fn index(self) -> usize {
        match self {
            Kind::Texture => 0,
            Kind::Model => 1,
            Kind::Audio => 2,
        }
    }
}

/// Every format the browser shows. FBX and `.blend` are left out of v1 on purpose.
const FORMATS: [(&str, Kind); 14] = [
    ("png", Kind::Texture),
    ("jpg", Kind::Texture),
    ("jpeg", Kind::Texture),
    ("bmp", Kind::Texture),
    ("tga", Kind::Texture),
    ("webp", Kind::Texture),
    ("glb", Kind::Model),
    ("gltf", Kind::Model),
    ("obj", Kind::Model),
    ("dae", Kind::Model),
    ("wav", Kind::Audio),
    ("ogg", Kind::Audio),
    ("mp3", Kind::Audio),
    ("flac", Kind::Audio),
];

fn format_of(path: &str) -> Option<(&'static str, Kind)> {
    let (_, ext) = path.rsplit_once('.')?;
    FORMATS.iter().find(|(name, _)| ext.eq_ignore_ascii_case(name)).copied()
}

#[derive(Clone, Debug)]
pub struct Asset {
    /// Relative to the library root, with forward slashes.
    pub rel: String,
    /// The file name, for display.
    pub name_at: u32,
    pub pack: u32,
    pub ext: &'static str,
    pub kind: Kind,
    pub size: u64,
    pub mtime: u64,
    /// Identifies this exact version of the file in the thumbnail cache.
    pub key: u64,
    /// Lowercase `rel`, what searches read.
    pub hay: String,
    /// A model that a better format of the same model in this pack stands in for.
    pub shadowed: bool,
}

impl Asset {
    pub fn name(&self) -> &str {
        &self.rel[self.name_at as usize..]
    }

    pub fn folder(&self) -> &str {
        self.rel[..self.name_at as usize].trim_end_matches('/')
    }
}

#[derive(Clone, Debug)]
pub struct Pack {
    pub name: String,
}

pub const LOOSE_PACK: &str = "Loose files";

#[derive(Default)]
pub struct Library {
    pub root: PathBuf,
    pub assets: Vec<Asset>,
    pub packs: Vec<Pack>,
}

/// One file the walk found.
pub struct Entry {
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
}

pub fn fnv(seed: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(seed, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3))
}

pub const FNV_SEED: u64 = 0xcbf2_9ce4_8422_2325;

fn key_of(rel: &str, size: u64, mtime: u64) -> u64 {
    let h = fnv(FNV_SEED, rel.as_bytes());
    let h = fnv(h, &size.to_le_bytes());
    fnv(h, &mtime.to_le_bytes())
}

/// Compares strings so `door_2` sorts before `door_10`.
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::Equal;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() { i += 1; }
            while j < b.len() && b[j].is_ascii_digit() { j += 1; }
            let trim = |s: &'_ [u8]| s.iter().position(|c| *c != b'0').map_or(0, |p| s.len() - p);
            let (la, lb) = (trim(&a[si..i]), trim(&b[sj..j]));
            let ord = la.cmp(&lb).then_with(|| a[i - la..i].cmp(&b[j - lb..j]));
            if ord != Equal { return ord; }
        } else {
            let ord = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if ord != Equal { return ord; }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

impl Library {
    /// Builds the index from walked files, in any order.
    pub fn build(root: PathBuf, mut files: Vec<Entry>) -> Library {
        files.retain(|f| format_of(&f.rel).is_some());
        files.sort_unstable_by(|a, b| natural(&a.rel, &b.rel));
        let mut packs: Vec<Pack> = Vec::new();
        let mut pack_ids: HashMap<String, u32> = HashMap::new();
        let mut assets = Vec::with_capacity(files.len());
        for f in files {
            let Some((ext, kind)) = format_of(&f.rel) else { continue };
            let pack_name = f.rel.split_once('/').map_or(LOOSE_PACK, |(p, _)| p).to_owned();
            let pack = *pack_ids.entry(pack_name.clone()).or_insert_with(|| {
                packs.push(Pack { name: pack_name });
                packs.len() as u32 - 1
            });
            let name_at = f.rel.rfind('/').map_or(0, |i| i + 1) as u32;
            let key = key_of(&f.rel, f.size, f.mtime);
            let hay = f.rel.to_lowercase();
            assets.push(Asset { rel: f.rel, name_at, pack, ext, kind, size: f.size, mtime: f.mtime, key, hay, shadowed: false });
        }
        mark_shadowed(&mut assets);
        Library { root, assets, packs }
    }

    pub fn path(&self, asset: &Asset) -> PathBuf {
        self.root.join(&asset.rel)
    }
}

/// Packs ship the same model as GLB, OBJ and DAE: keep the most self-contained one in view.
fn mark_shadowed(assets: &mut [Asset]) {
    fn rank(ext: &str) -> u8 {
        match ext { "glb" => 0, "gltf" => 1, "obj" => 2, _ => 3 }
    }
    let mut best: HashMap<(u32, String), (u8, usize)> = HashMap::new();
    for (i, a) in assets.iter().enumerate().filter(|(_, a)| a.kind == Kind::Model) {
        let stem = a.name().rsplit_once('.').map_or(a.name(), |(s, _)| s).to_lowercase();
        let slot = best.entry((a.pack, stem)).or_insert((u8::MAX, i));
        if rank(a.ext) < slot.0 { *slot = (rank(a.ext), i); }
    }
    let keep: std::collections::HashSet<usize> = best.into_values().map(|(_, i)| i).collect();
    for (i, a) in assets.iter_mut().enumerate() {
        a.shadowed = a.kind == Kind::Model && !keep.contains(&i);
    }
}

/// Walks `root`. Unreadable folders are skipped.
pub fn walk(root: &Path) -> Vec<Entry> {
    let mut files = Vec::new();
    for e in walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !e.file_type().is_file() { continue }
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if format_of(&rel).is_none() { continue }
        let Ok(meta) = e.metadata() else { continue };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        files.push(Entry { rel, size: meta.len(), mtime });
    }
    files
}

/// Writes the walked files so the next launch has its list before the disk is walked again.
pub fn save_snapshot(path: &Path, lib: &Library) -> std::io::Result<()> {
    use std::fmt::Write;
    let mut out = String::with_capacity(lib.assets.len() * 80);
    let _ = writeln!(out, "{}", lib.root.display());
    for a in &lib.assets {
        let _ = writeln!(out, "{}\t{}\t{}", a.size, a.mtime, a.rel);
    }
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)
}

/// The snapshot for `root`, if there is one of it.
pub fn load_snapshot(path: &Path, root: &Path) -> Option<Library> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if Path::new(lines.next()?) != root { return None; }
    let files = lines.filter_map(|l| {
        let mut parts = l.splitn(3, '\t');
        let size = parts.next()?.parse().ok()?;
        let mtime = parts.next()?.parse().ok()?;
        Some(Entry { rel: parts.next()?.to_owned(), size, mtime })
    }).collect();
    Some(Library::build(root.to_path_buf(), files))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Pack,
    Name,
    Largest,
    Newest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub text: String,
    pub kind: Option<Kind>,
    pub pack: Option<u32>,
    pub ext: Option<&'static str>,
    pub all_formats: bool,
    pub sort: Sort,
}

impl Default for Filter {
    fn default() -> Self {
        Filter { text: String::new(), kind: None, pack: None, ext: None, all_formats: false, sort: Sort::Pack }
    }
}

/// A filter's answer, with counts that ignore the facet they label so a facet's other choices
/// still say what they would give.
#[derive(Default)]
pub struct Results {
    pub ids: Vec<u32>,
    pub by_kind: [u32; 3],
    pub any_kind: u32,
    pub by_pack: Vec<u32>,
    pub any_pack: u32,
    pub by_ext: Vec<(&'static str, u32)>,
}

pub fn search(lib: &Library, f: &Filter) -> Results {
    let lowered = f.text.to_lowercase();
    let (mut want, mut reject) = (Vec::new(), Vec::new());
    for token in lowered.split_whitespace() {
        match token.strip_prefix('-') {
            Some(t) if !t.is_empty() => reject.push(t),
            Some(_) => {}
            None => want.push(token),
        }
    }
    let mut out = Results { by_pack: vec![0; lib.packs.len()], ..Results::default() };
    let mut ext_counts = [0u32; FORMATS.len()];
    for (i, a) in lib.assets.iter().enumerate() {
        if a.shadowed && !f.all_formats && f.ext != Some(a.ext) { continue }
        if !want.iter().all(|t| a.hay.contains(t)) || reject.iter().any(|t| a.hay.contains(t)) { continue }
        let kind_ok = f.kind.is_none_or(|k| k == a.kind);
        let pack_ok = f.pack.is_none_or(|p| p == a.pack);
        let ext_ok = f.ext.is_none_or(|e| e == a.ext);
        if pack_ok && ext_ok {
            out.by_kind[a.kind.index()] += 1;
            out.any_kind += 1;
        }
        if kind_ok && ext_ok {
            out.by_pack[a.pack as usize] += 1;
            out.any_pack += 1;
        }
        if kind_ok && pack_ok {
            if let Some(slot) = FORMATS.iter().position(|(e, _)| *e == a.ext) { ext_counts[slot] += 1; }
            if ext_ok { out.ids.push(i as u32); }
        }
    }
    out.by_ext = FORMATS.iter().zip(ext_counts).filter(|(_, n)| *n > 0).map(|((e, _), n)| (*e, n)).collect();
    out.by_ext.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let at = |i: &u32| &lib.assets[*i as usize];
    match f.sort {
        Sort::Pack => {}
        Sort::Name => out.ids.sort_by(|a, b| natural(at(a).name(), at(b).name())),
        Sort::Largest => out.ids.sort_by_key(|i| std::cmp::Reverse(at(i).size)),
        Sort::Newest => out.ids.sort_by_key(|i| std::cmp::Reverse(at(i).mtime)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rel: &str) -> Entry {
        Entry { rel: rel.into(), size: 10, mtime: 5 }
    }

    fn lib() -> Library {
        Library::build(PathBuf::from("C:/lib"), vec![
            entry("Mega/Models/GLB/door_2.glb"),
            entry("Mega/Models/OBJ/door_2.obj"),
            entry("Mega/Models/GLB/door_10.glb"),
            entry("Mega/Tex/brick.png"),
            entry("Sfx/door creak.WAV"),
            entry("Sfx/readme.txt"),
            entry("Sfx/thing.fbx"),
            entry("loose.glb"),
        ])
    }

    fn names(l: &Library, r: &Results) -> Vec<String> {
        r.ids.iter().map(|i| l.assets[*i as usize].name().to_owned()).collect()
    }

    #[test]
    fn unsupported_files_are_left_out_and_loose_files_get_a_pack() {
        let l = lib();
        assert_eq!(l.assets.len(), 6);
        assert!(l.packs.iter().any(|p| p.name == LOOSE_PACK));
        assert!(!l.assets.iter().any(|a| a.name().ends_with("fbx")));
    }

    #[test]
    fn natural_order_counts_numbers() {
        let l = lib();
        let r = search(&l, &Filter { text: "models".into(), all_formats: true, ..Filter::default() });
        assert_eq!(names(&l, &r), ["door_2.glb", "door_10.glb", "door_2.obj"]);
    }

    #[test]
    fn duplicate_formats_collapse_to_the_best_one_unless_asked() {
        let l = lib();
        let r = search(&l, &Filter { text: "door_2".into(), ..Filter::default() });
        assert_eq!(names(&l, &r), ["door_2.glb"]);
        let r = search(&l, &Filter { text: "door_2".into(), ext: Some("obj"), ..Filter::default() });
        assert_eq!(names(&l, &r), ["door_2.obj"], "asking for the format shows it");
    }

    #[test]
    fn tokens_all_match_and_a_minus_excludes() {
        let l = lib();
        let r = search(&l, &Filter { text: "DOOR -creak mega".into(), ..Filter::default() });
        assert_eq!(names(&l, &r), ["door_2.glb", "door_10.glb"]);
    }

    #[test]
    fn a_facets_counts_ignore_that_facet() {
        let l = lib();
        let r = search(&l, &Filter { kind: Some(Kind::Audio), ..Filter::default() });
        assert_eq!(r.ids.len(), 1);
        assert_eq!(r.by_kind, [1, 3, 1], "other kinds still count");
        assert_eq!(r.any_pack, 1, "packs follow the chosen kind");
    }

    #[test]
    fn a_new_version_of_a_file_gets_a_new_key() {
        assert_ne!(key_of("a.png", 1, 1), key_of("a.png", 1, 2));
        assert_ne!(key_of("a.png", 1, 1), key_of("a.png", 2, 1));
    }
}
