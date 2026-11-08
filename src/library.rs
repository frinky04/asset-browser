//! The asset index: walking a root folder, remembering it between runs, and answering searches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::query::Query;
use crate::smart::Hit;

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

/// Every format the browser shows. `.blend` is left out on purpose.
const FORMATS: [(&str, Kind); 15] = [
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
    ("fbx", Kind::Model),
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
    /// `rel` split into lowercase words: `Props/woodDoor_2.obj` is `props/wood door 2 obj`.
    pub words: String,
    /// The texture map its name says it is (`normal`, `roughness`, `color`...), or `""`.
    pub map: &'static str,
    /// A model that a better format of the same model in this pack stands in for.
    pub shadowed: bool,
}

impl Asset {
    pub fn name(&self) -> &str {
        &self.rel[self.name_at as usize..]
    }

    /// The file name without its extension.
    pub fn stem(&self) -> &str {
        self.name().rsplit_once('.').map_or(self.name(), |(stem, _)| stem)
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
pub const LOOSE_PACK_LOWER: &str = "loose files";

#[derive(Default)]
pub struct Library {
    pub root: PathBuf,
    pub assets: Vec<Asset>,
    pub packs: Vec<Pack>,
    vocabulary: std::sync::OnceLock<Vec<(String, u32)>>,
}

/// `text` as lowercase words: punctuation, camelCase and letter/digit boundaries become single
/// spaces. Slashes stay, so a phrase can't match across folders.
pub fn words_of(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut previous = ' ';
    for c in text.chars() {
        if c == '/' {
            if out.ends_with(' ') { out.pop(); }
            out.push('/');
            previous = '/';
        } else if c.is_alphanumeric() {
            let boundary = (previous.is_lowercase() && c.is_uppercase()) || (previous.is_alphabetic() && c.is_numeric()) || (previous.is_numeric() && c.is_alphabetic());
            if boundary && !out.ends_with(' ') { out.push(' '); }
            out.extend(c.to_lowercase());
            previous = c;
        } else if !out.is_empty() && !out.ends_with([' ', '/']) {
            out.push(' ');
            previous = ' ';
        }
    }
    if out.ends_with(' ') { out.pop(); }
    out
}

pub fn known_ext(ext: &str) -> Option<&'static str> {
    FORMATS.iter().find(|(name, _)| ext.eq_ignore_ascii_case(name)).map(|(name, _)| *name)
}

pub fn known_exts() -> impl Iterator<Item = &'static str> {
    FORMATS.iter().map(|(name, _)| *name)
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
            // Bumped when model thumbnails change meaning, so cached ones are redrawn.
            let key = if kind == Kind::Model { fnv(key, b"model-thumbs-2") } else { key };
            let hay = f.rel.to_lowercase();
            let words = words_of(&f.rel);
            let map = if kind == Kind::Texture { crate::texture_guess::map_named_by(&f.rel[name_at as usize..]) } else { "" };
            assets.push(Asset { rel: f.rel, name_at, pack, ext, kind, size: f.size, mtime: f.mtime, key, hay, words, map, shadowed: false });
        }
        mark_shadowed(&mut assets);
        confirm_ambiguous_maps(&mut assets);
        Library { root, assets, packs, vocabulary: std::sync::OnceLock::new() }
    }

    /// Every word in the library with how often it's used, for correcting typos. Only built
    /// once a search comes up empty.
    fn vocabulary(&self) -> &[(String, u32)] {
        self.vocabulary.get_or_init(|| {
            let mut counts: HashMap<&str, u32> = HashMap::new();
            for a in &self.assets {
                for word in a.words.split([' ', '/']).filter(|w| w.len() >= 3 && w.chars().all(char::is_alphabetic)) {
                    *counts.entry(word).or_default() += 1;
                }
            }
            counts.into_iter().map(|(w, n)| (w.to_owned(), n)).collect()
        })
    }

    pub fn path(&self, asset: &Asset) -> PathBuf {
        self.root.join(&asset.rel)
    }
}

/// `metal_6.png` is a picture of metal unless another map of the same surface sits beside it.
fn confirm_ambiguous_maps(assets: &mut [Asset]) {
    use crate::texture_guess::{map_name_is_ambiguous, map_surface};
    let ambiguous: Vec<usize> = assets.iter().enumerate().filter(|(_, a)| !a.map.is_empty() && map_name_is_ambiguous(a.name())).map(|(i, _)| i).collect();
    let folders: std::collections::HashSet<&str> = ambiguous.iter().map(|i| assets[*i].folder()).collect();
    // Two variants of one picture aren't a set: it takes maps of different kinds.
    let mut kinds: HashMap<(&str, String), std::collections::HashSet<&'static str>> = HashMap::new();
    for a in assets.iter().filter(|a| !a.map.is_empty() && folders.contains(a.folder())) {
        kinds.entry((a.folder(), map_surface(a.name()))).or_default().insert(a.map);
    }
    let lonely: Vec<usize> = ambiguous.into_iter().filter(|i| {
        let surface = map_surface(assets[*i].name());
        surface.is_empty() || kinds.get(&(assets[*i].folder(), surface)).is_none_or(|k| k.len() < 2)
    }).collect();
    for i in lonely { assets[i].map = ""; }
}

/// Packs ship the same model as GLB, OBJ, DAE and FBX: keep the most self-contained one in view.
fn mark_shadowed(assets: &mut [Asset]) {
    fn rank(ext: &str) -> u8 {
        match ext { "glb" => 0, "gltf" => 1, "obj" => 2, "dae" => 3, _ => 4 }
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
    /// The words searched for instead, when what was typed found nothing.
    pub corrected: Option<String>,
    /// The answer can grow as thumbnailing measures more files.
    pub uses_facts: bool,
}

/// [`search_with`], knowing nothing that thumbnailing measures.
#[cfg(test)]
pub fn search(lib: &Library, f: &Filter) -> Results {
    search_with(lib, f, &|_| None)
}

/// [`search_described`], with no descriptions to go on.
#[cfg(test)]
pub fn search_with(lib: &Library, f: &Filter, facts: &dyn Fn(u64) -> Option<crate::thumbs::Facts>) -> Results {
    search_described(lib, f, facts, &[])
}

/// What to look for by meaning in the search box's text, if anything.
pub fn description_in(text: &str) -> Option<String> {
    Query::parse(text, 0).description()
}

/// How far below its best a kind's matches may fall, and how many of them are shown.
const DESCRIBED_WINDOW: f32 = 0.07;
const MOST_DESCRIBED: usize = 60;
/// Ranks files found by description after everything matched by name.
const DESCRIBED_FIT: u32 = 1_000_000;

/// Of `hits`, best first, the ones this search would show, as the place each comes in.
fn described(lib: &Library, f: &Filter, query: &Query, facts: &dyn Fn(u64) -> Option<crate::thumbs::Facts>, hits: &[Hit]) -> HashMap<u32, u32> {
    if hits.is_empty() { return HashMap::new() }
    let rank_of: HashMap<u64, usize> = hits.iter().enumerate().map(|(rank, h)| (h.key, rank)).collect();
    let mut found: Vec<(usize, u32)> = lib.assets.iter().enumerate()
        .filter_map(|(i, a)| Some((*rank_of.get(&a.key)?, i as u32, a)))
        .filter(|(_, _, a)| f.pack.is_none_or(|p| p == a.pack) && f.ext.is_none_or(|e| e == a.ext) && query.allows(a, facts))
        .map(|(rank, i, _)| (rank, i))
        .collect();
    found.sort_unstable();
    let mut best = [None; 3];
    let mut taken = [0usize; 3];
    let mut places = HashMap::new();
    for (rank, i) in found {
        let kind = lib.assets[i as usize].kind.index();
        let score = hits[rank].score;
        let top = *best[kind].get_or_insert(score);
        if score < top - DESCRIBED_WINDOW || taken[kind] >= MOST_DESCRIBED { continue }
        taken[kind] += 1;
        places.insert(i, rank as u32);
    }
    places
}

/// The assets `f` asks for: those whose names match, then those whose look matched the search
/// text. `facts` says what's been measured about a file, by its key.
pub fn search_described(lib: &Library, f: &Filter, facts: &dyn Fn(u64) -> Option<crate::thumbs::Facts>, hits: &[Hit]) -> Results {
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let query = Query::parse(&f.text, now);
    let places = described(lib, f, &query, facts, hits);
    let found = run(lib, f, &query, facts, &places);
    if found.ids.is_empty() && let Some((fixed, words)) = query.corrected(lib.vocabulary()) {
        let mut second = run(lib, f, &fixed, facts, &HashMap::new());
        if !second.ids.is_empty() {
            second.corrected = Some(words);
            return second;
        }
    }
    found
}

fn run(lib: &Library, f: &Filter, query: &Query, facts: &dyn Fn(u64) -> Option<crate::thumbs::Facts>, places: &HashMap<u32, u32>) -> Results {
    let mut out = Results { by_pack: vec![0; lib.packs.len()], uses_facts: query.needs_facts(), ..Results::default() };
    let mut ext_counts = [0u32; FORMATS.len()];
    let rank = f.sort == Sort::Pack && query.has_words();
    let mut fits: Vec<u32> = Vec::new();
    for (i, a) in lib.assets.iter().enumerate() {
        if a.shadowed && !f.all_formats && f.ext != Some(a.ext) && !query.asks_for_ext(a.ext) { continue }
        let Some(fit) = query.fit(a, facts).or_else(|| places.get(&(i as u32)).map(|place| DESCRIBED_FIT + place)) else { continue };
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
            if ext_ok {
                out.ids.push(i as u32);
                if rank { fits.push(fit); }
            }
        }
    }
    if rank {
        // Best fits first; among equals the library's own order stands.
        let mut ranked: Vec<(u32, u32)> = fits.into_iter().zip(out.ids.iter().copied()).collect();
        ranked.sort_by_key(|(fit, _)| *fit);
        out.ids = ranked.into_iter().map(|(_, id)| id).collect();
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
            entry("Sfx/thing.blend"),
            entry("loose.glb"),
        ])
    }

    fn names(l: &Library, r: &Results) -> Vec<String> {
        r.ids.iter().map(|i| l.assets[*i as usize].name().to_owned()).collect()
    }

    fn hit(l: &Library, name: &str, score: f32) -> Hit {
        Hit { key: l.assets.iter().find(|a| a.name() == name).unwrap().key, score }
    }

    #[test]
    fn a_description_finds_files_after_the_ones_named_that_way() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("P/old_door.png"), entry("P/T_0012.png"), entry("P/T_0013.png")]);
        let hits = [hit(&l, "T_0013.png", 0.8), hit(&l, "old_door.png", 0.75), hit(&l, "T_0012.png", 0.74)];
        let r = search_described(&l, &Filter { text: "door".into(), ..Filter::default() }, &|_| None, &hits);
        assert_eq!(names(&l, &r), ["old_door.png", "T_0013.png", "T_0012.png"]);
    }

    #[test]
    fn a_description_fills_in_what_names_alone_found_nothing_for() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("P/T_0012.png"), entry("P/T_0013.png")]);
        let hits = [hit(&l, "T_0012.png", 0.8)];
        let r = search_described(&l, &Filter { text: "something to sit on".into(), ..Filter::default() }, &|_| None, &hits);
        assert_eq!(names(&l, &r), ["T_0012.png"]);
        assert_eq!(r.corrected, None);
    }

    #[test]
    fn fields_and_exclusions_still_apply_to_files_found_by_description() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("Keep/T_1.png"), entry("Other/T_2.png"), entry("Keep/rusty_3.png")]);
        let hits = [hit(&l, "T_2.png", 0.8), hit(&l, "T_1.png", 0.78), hit(&l, "rusty_3.png", 0.77)];
        let r = search_described(&l, &Filter { text: "wall pack:keep -rusty".into(), ..Filter::default() }, &|_| None, &hits);
        assert_eq!(names(&l, &r), ["T_1.png"]);
    }

    #[test]
    fn matches_far_below_the_best_of_their_kind_are_dropped() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("P/a_1.png"), entry("P/b_2.png"), entry("P/c_3.glb")]);
        let hits = [hit(&l, "a_1.png", 0.80), hit(&l, "b_2.png", 0.70), hit(&l, "c_3.glb", 0.68)];
        let r = search_described(&l, &Filter { text: "zzz".into(), ..Filter::default() }, &|_| None, &hits);
        assert_eq!(names(&l, &r), ["a_1.png", "c_3.glb"], "the model is judged against other models");
    }

    #[test]
    fn counts_include_what_a_description_found() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("P/a_1.png"), entry("P/b_2.glb")]);
        let hits = [hit(&l, "a_1.png", 0.8), hit(&l, "b_2.glb", 0.8)];
        let r = search_described(&l, &Filter { text: "zzz".into(), kind: Some(Kind::Texture), ..Filter::default() }, &|_| None, &hits);
        assert_eq!(names(&l, &r), ["a_1.png"]);
        assert_eq!(r.by_kind[Kind::Model.index()], 1, "the tab still shows what's waiting there");
    }

    #[test]
    fn only_free_words_are_a_description() {
        assert_eq!(description_in("old hospital tiles pack:mega -rust").as_deref(), Some("old hospital tiles"));
        assert_eq!(description_in("is:model size:>1mb"), None);
        assert_eq!(description_in("\"exact door\""), None, "quotes ask for those words only");
        assert_eq!(description_in("ab"), None, "too short to mean anything");
    }

    #[test]
    fn unsupported_files_are_left_out_and_loose_files_get_a_pack() {
        let l = lib();
        assert_eq!(l.assets.len(), 6);
        assert!(l.packs.iter().any(|p| p.name == LOOSE_PACK));
        assert!(!l.assets.iter().any(|a| a.name().ends_with("blend")));
    }

    #[test]
    fn an_fbx_is_a_model_that_only_shows_when_no_other_format_of_it_does() {
        let l = Library::build(PathBuf::from("C:/lib"), vec![entry("P/door.dae"), entry("P/door.FBX"), entry("P/lever.fbx")]);
        let lever = l.assets.iter().find(|a| a.name() == "lever.fbx").unwrap();
        assert_eq!((lever.ext, lever.kind), ("fbx", Kind::Model));
        assert_eq!(names(&l, &search(&l, &Filter::default())), ["door.dae", "lever.fbx"]);
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
