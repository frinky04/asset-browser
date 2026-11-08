//! Thumbnails: an append-only cache on disk, and workers that fill it and feed the grid.
//!
//! Workers write straight into each tile's `Pixels`, so a thumbnail landing costs no view run.
//! What's on screen goes first; idle workers crawl the rest of the library, so later scrolling
//! finds everything already made.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use rune::{Live, Pixels};

use crate::library::{Asset, Kind, Library};
use crate::{model, raster, wave};

pub const THUMB: u32 = 256;
const WAVE_HEIGHT: u32 = 128;
pub const WAVE_COLOR: [u8; 3] = [206, 206, 214];
/// Decoded thumbnails kept in memory. A full one is 256 KB.
const MAX_LOADED: usize = 1400;

/// What a thumbnail says about its source.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Facts {
    /// Pixel size for textures; triangle count in `width` for models.
    pub width: u32,
    pub height: u32,
    pub seconds: f32,
    pub failed: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    offset: u64,
    len: u32,
    facts: Facts,
}

const INDEX_RECORD: usize = 8 + 8 + 4 + 4 + 4 + 4 + 1;

/// The on-disk cache: `thumbs.pack` holds QOI images, `thumbs.idx` fixed records pointing into it.
struct Store {
    pack_path: PathBuf,
    index_path: PathBuf,
    pack: File,
    index: Mutex<(File, File, u64)>,
    entries: RwLock<HashMap<u64, Entry>>,
}

impl Store {
    fn open(dir: &Path) -> std::io::Result<Store> {
        std::fs::create_dir_all(dir)?;
        let pack_path = dir.join("thumbs.pack");
        let index_path = dir.join("thumbs.idx");
        let writer = OpenOptions::new().create(true).append(true).open(&pack_path)?;
        let index_writer = OpenOptions::new().create(true).append(true).open(&index_path)?;
        let mut pack_len = writer.metadata()?.len();
        let mut entries = HashMap::new();
        let raw = std::fs::read(&index_path).unwrap_or_default();
        let whole = raw.len() - raw.len() % INDEX_RECORD;
        for r in raw[..whole].chunks_exact(INDEX_RECORD) {
            let u64_at = |i: usize| u64::from_le_bytes(r[i..i + 8].try_into().unwrap_or([0; 8]));
            let u32_at = |i: usize| u32::from_le_bytes(r[i..i + 4].try_into().unwrap_or([0; 4]));
            let (offset, len) = (u64_at(8), u32_at(16));
            // Records whose image never fully reached the pack are treated as absent.
            if offset + u64::from(len) > pack_len { continue }
            let facts = Facts { width: u32_at(20), height: u32_at(24), seconds: f32::from_bits(u32_at(28)), failed: r[32] != 0 };
            entries.insert(u64_at(0), Entry { offset, len, facts });
        }
        if raw.len() != whole {
            // A torn record: drop it so appends stay aligned.
            OpenOptions::new().write(true).open(&index_path)?.set_len(whole as u64)?;
        }
        pack_len = pack_len.max(entries.values().map(|e| e.offset + u64::from(e.len)).max().unwrap_or(0));
        Ok(Store {
            pack: File::open(&pack_path)?,
            pack_path,
            index_path,
            index: Mutex::new((writer, index_writer, pack_len)),
            entries: RwLock::new(entries),
        })
    }

    /// Empties the cache, so every thumbnail is made again.
    fn reset(&self) {
        let Ok(mut guard) = self.index.lock() else { return };
        // The handles appending to these can't truncate them.
        for path in [&self.pack_path, &self.index_path] {
            if let Ok(file) = OpenOptions::new().write(true).open(path) { let _ = file.set_len(0); }
        }
        guard.2 = 0;
        if let Ok(mut entries) = self.entries.write() { entries.clear(); }
    }

    fn get(&self, key: u64) -> Option<Entry> {
        self.entries.read().ok()?.get(&key).copied()
    }

    fn read(&self, entry: &Entry) -> Option<(u32, u32, Vec<u8>)> {
        if entry.len == 0 { return None; }
        let mut bytes = vec![0u8; entry.len as usize];
        read_at(&self.pack, &mut bytes, entry.offset).ok()?;
        let (header, rgba) = qoi::decode_to_vec(&bytes).ok()?;
        Some((header.width, header.height, rgba))
    }

    fn put(&self, key: u64, image: Option<&[u8]>, facts: Facts) {
        let Ok(mut guard) = self.index.lock() else { return };
        let (pack, index, end) = &mut *guard;
        let offset = *end;
        let bytes = image.unwrap_or(&[]);
        if pack.write_all(bytes).is_err() { return }
        *end += bytes.len() as u64;
        let mut record = Vec::with_capacity(INDEX_RECORD);
        record.extend_from_slice(&key.to_le_bytes());
        record.extend_from_slice(&offset.to_le_bytes());
        record.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        record.extend_from_slice(&facts.width.to_le_bytes());
        record.extend_from_slice(&facts.height.to_le_bytes());
        record.extend_from_slice(&facts.seconds.to_bits().to_le_bytes());
        record.push(u8::from(facts.failed));
        if index.write_all(&record).is_err() { return }
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(key, Entry { offset, len: bytes.len() as u32, facts });
        }
    }
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.seek_read(&mut buf[filled..], offset)?;
        if n == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
        filled += n;
        offset += n as u64;
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

struct Slot {
    pixels: Pixels,
    /// 0 until the thumbnail arrives, so it can fade in without a view run.
    fade: Live<f32>,
    loaded: bool,
    /// A waveform, which is drawn in the theme's colour.
    audio: bool,
    stamp: u64,
}

#[derive(Default)]
struct Queue {
    /// Asset indices wanted now, most important first.
    wanted: VecDeque<u32>,
    inflight: HashSet<u64>,
    /// Where the background crawl has got to.
    crawl: usize,
    generation: u64,
}

struct Shared {
    store: Option<Store>,
    library: RwLock<Arc<Library>>,
    queue: Mutex<Queue>,
    work: Condvar,
    slots: Mutex<(HashMap<u64, Slot>, u64)>,
    done: AtomicUsize,
    quit: AtomicBool,
    /// The colour waveforms are shown in, as 0xRRGGBB.
    wave_color: AtomicU32,
}

#[derive(Clone)]
pub struct Thumbs {
    shared: Arc<Shared>,
}

impl Thumbs {
    pub fn new(dir: PathBuf) -> Thumbs {
        let store = Store::open(&dir).map_err(|e| eprintln!("thumbnail cache unavailable: {e}")).ok();
        let shared = Arc::new(Shared {
            store,
            library: RwLock::new(Arc::new(Library::default())),
            queue: Mutex::new(Queue::default()),
            work: Condvar::new(),
            slots: Mutex::new((HashMap::new(), 0)),
            done: AtomicUsize::new(0),
            quit: AtomicBool::new(false),
            wave_color: AtomicU32::new(pack_rgb(WAVE_COLOR)),
        });
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        for i in 0..cores.div_ceil(2).clamp(2, 10) {
            let shared = shared.clone();
            let crawls = i < cores.div_ceil(4).clamp(1, 6);
            let _ = std::thread::Builder::new().name(format!("thumb-{i}")).spawn(move || worker(&shared, crawls));
        }
        Thumbs { shared }
    }

    /// Points the workers at a new index and starts the crawl over.
    pub fn set_library(&self, library: Arc<Library>) {
        if let Ok(mut l) = self.shared.library.write() { *l = library.clone(); }
        let made = self.shared.store.as_ref().and_then(|s| s.entries.read().ok().map(|e| {
            library.assets.iter().filter(|a| e.contains_key(&a.key)).count()
        })).unwrap_or(0);
        self.shared.done.store(made, Ordering::Relaxed);
        if let Ok(mut q) = self.shared.queue.lock() {
            q.wanted.clear();
            q.crawl = 0;
            q.generation += 1;
        }
        self.shared.work.notify_all();
    }

    /// The tile's image and its opacity, both empty until its thumbnail arrives. Handles are
    /// stable per file version.
    pub fn tile(&self, key: u64) -> (Pixels, Live<f32>) {
        let Ok(mut guard) = self.shared.slots.lock() else { return (Pixels::new(), Live::new(1.0)) };
        let (slots, clock) = &mut *guard;
        *clock += 1;
        let stamp = *clock;
        let slot = slots.entry(key).or_insert_with(|| Slot { pixels: Pixels::new(), fade: Live::new(0.0), loaded: false, audio: false, stamp });
        slot.stamp = stamp;
        (slot.pixels.clone(), slot.fade.clone())
    }

    /// Sets the colour waveforms show in, dropping the ones already loaded in another.
    pub fn set_wave_color(&self, color: [u8; 3]) {
        if self.shared.wave_color.swap(pack_rgb(color), Ordering::Relaxed) == pack_rgb(color) { return }
        if let Ok(mut guard) = self.shared.slots.lock() { guard.0.retain(|_, slot| !slot.audio); }
    }

    /// Forgets every thumbnail, on disk and in memory, so they're all made again.
    pub fn rebuild(&self) {
        if let Some(store) = &self.shared.store { store.reset(); }
        if let Ok(mut guard) = self.shared.slots.lock() { guard.0.clear(); }
        self.shared.done.store(0, Ordering::Relaxed);
    }

    /// Replaces what's wanted now. Called as the grid scrolls, so it stays cheap.
    pub fn want(&self, ids: Vec<u32>) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.wanted = ids.into();
        }
        self.shared.work.notify_all();
        self.evict();
    }

    fn evict(&self) {
        let Ok(mut guard) = self.shared.slots.lock() else { return };
        let (slots, _) = &mut *guard;
        if slots.len() <= MAX_LOADED { return }
        let mut stamps: Vec<u64> = slots.values().map(|s| s.stamp).collect();
        let cut = slots.len() - MAX_LOADED * 3 / 4;
        let (_, nth, _) = stamps.select_nth_unstable(cut);
        let oldest_kept = *nth;
        slots.retain(|_, s| s.stamp >= oldest_kept);
    }

    pub fn facts(&self, key: u64) -> Option<Facts> {
        Some(self.shared.store.as_ref()?.get(key)?.facts)
    }

    /// Decoded RGBA of a finished thumbnail (for a drag image).
    pub fn image(&self, key: u64) -> Option<(u32, u32, Vec<u8>)> {
        let store = self.shared.store.as_ref()?;
        store.read(&store.get(key)?)
    }

    /// (thumbnails made, files in the library).
    pub fn progress(&self) -> (usize, usize) {
        let total = self.shared.library.read().map_or(0, |l| l.assets.len());
        (self.shared.done.load(Ordering::Relaxed).min(total), total)
    }

    /// Blocks until everything wanted now has landed, for deterministic headless runs.
    pub fn settle(&self, timeout: std::time::Duration) {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            let idle = self.shared.queue.lock().is_ok_and(|q| q.wanted.is_empty() && q.inflight.is_empty());
            if idle { return }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }

    /// Whether anything wanted now is still on its way.
    pub fn busy(&self) -> bool {
        self.shared.queue.lock().is_ok_and(|q| !q.wanted.is_empty() || !q.inflight.is_empty())
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
    }
}

enum Job {
    Show(u32),
    Crawl(u32),
}

fn next_job(shared: &Shared, crawls: bool) -> Option<(Job, u64, Arc<Library>)> {
    let mut q = shared.queue.lock().ok()?;
    loop {
        if shared.quit.load(Ordering::Relaxed) { return None; }
        let library = shared.library.read().ok()?.clone();
        while let Some(id) = q.wanted.pop_front() {
            let Some(asset) = library.assets.get(id as usize) else { continue };
            let loaded = shared.slots.lock().ok()?.0.get(&asset.key).is_some_and(|s| s.loaded);
            if loaded || !q.inflight.insert(asset.key) { continue }
            return Some((Job::Show(id), asset.key, library));
        }
        if crawls {
            while let Some(asset) = library.assets.get(q.crawl) {
                let at = q.crawl as u32;
                q.crawl += 1;
                let exists = shared.store.as_ref().is_none_or(|s| s.get(asset.key).is_some());
                if exists || !q.inflight.insert(asset.key) { continue }
                return Some((Job::Crawl(at), asset.key, library));
            }
        }
        q = shared.work.wait(q).ok()?;
    }
}

fn worker(shared: &Arc<Shared>, crawls: bool) {
    while let Some((job, key, library)) = next_job(shared, crawls) {
        let (id, show) = match job {
            Job::Show(id) => (id, true),
            Job::Crawl(id) => (id, false),
        };
        let asset = &library.assets[id as usize];
        let cached = shared.store.as_ref().and_then(|s| s.get(key));
        let mut image = match (&cached, &shared.store) {
            (Some(entry), Some(store)) => if show { store.read(entry) } else { None },
            _ => {
                let made = make(&library.path(asset), &library.root, asset);
                let encoded = made.image.as_ref().and_then(|(w, h, rgba)| qoi::encode_to_vec(rgba, *w, *h).ok());
                if let Some(store) = &shared.store {
                    store.put(key, encoded.as_deref(), made.facts);
                    shared.done.fetch_add(1, Ordering::Relaxed);
                }
                made.image
            }
        };
        let audio = asset.kind == Kind::Audio;
        if audio && let Some((_, _, rgba)) = &mut image {
            let [r, g, b] = unpack_rgb(shared.wave_color.load(Ordering::Relaxed));
            for px in rgba.chunks_exact_mut(4) { (px[0], px[1], px[2]) = (r, g, b); }
        }
        // A crawl can make a thumbnail a tile is waiting on, so either kind of job delivers it.
        if show || image.is_some() {
            if let Ok(mut guard) = shared.slots.lock() {
                if let Some(slot) = guard.0.get_mut(&key).filter(|s| show || !s.loaded) {
                    slot.loaded = true;
                    slot.audio = audio;
                    if let Some((w, h, rgba)) = image { slot.pixels.set(w, h, rgba); }
                    slot.fade.set(1.0);
                }
            }
        }
        if let Ok(mut q) = shared.queue.lock() { q.inflight.remove(&key); }
    }
}

fn pack_rgb([r, g, b]: [u8; 3]) -> u32 {
    u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

fn unpack_rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

struct Made {
    image: Option<(u32, u32, Vec<u8>)>,
    facts: Facts,
}

fn failed() -> Made {
    Made { image: None, facts: Facts { failed: true, ..Facts::default() } }
}

/// Makes a thumbnail from the source file, surviving whatever a corrupt file does to a decoder.
fn make(path: &Path, library: &Path, asset: &Asset) -> Made {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match asset.kind {
        Kind::Texture => texture_thumb(path),
        Kind::Model => model_thumb(path, library),
        Kind::Audio => audio_thumb(path),
    }));
    result.ok().flatten().unwrap_or_else(failed)
}

fn texture_thumb(path: &Path) -> Option<Made> {
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut img = reader.decode().ok()?;
    let (width, height) = (img.width(), img.height());
    let longest = width.max(height).max(1);
    if longest > THUMB {
        img = img.thumbnail(THUMB, THUMB);
    } else if THUMB / longest >= 2 {
        // Tiles are bigger than most PSX textures, and the grid smooths what it enlarges: a
        // whole-number enlargement here keeps their pixels hard.
        let factor = THUMB / longest;
        img = img.resize_exact(width * factor, height * factor, image::imageops::FilterType::Nearest);
    }
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(Made { image: Some((w, h, rgba.into_raw())), facts: Facts { width, height, ..Facts::default() } })
}

fn model_thumb(path: &Path, library: &Path) -> Option<Made> {
    let m = model::load(path, library).ok()?;
    let rgba = raster::render_thumb(&m, THUMB);
    Some(Made { image: Some((THUMB, THUMB, rgba)), facts: Facts { width: m.triangles() as u32, ..Facts::default() } })
}

fn audio_thumb(path: &Path) -> Option<Made> {
    let peaks = wave::peaks(path, THUMB as usize).ok()?;
    let rgba = wave::draw(&peaks.values, THUMB, WAVE_HEIGHT, WAVE_COLOR);
    Some(Made { image: Some((THUMB, WAVE_HEIGHT, rgba)), facts: Facts { seconds: peaks.seconds, ..Facts::default() } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ab-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_store_reads_back_after_reopening_and_ignores_a_torn_tail() {
        let d = dir("store");
        let rgba = vec![1u8, 2, 3, 255].repeat(4);
        let encoded = qoi::encode_to_vec(&rgba, 2, 2).unwrap();
        {
            let store = Store::open(&d).unwrap();
            store.put(7, Some(&encoded), Facts { width: 2048, height: 1024, seconds: 0.0, failed: false });
            store.put(8, None, Facts { failed: true, ..Facts::default() });
        }
        let mut idx = OpenOptions::new().append(true).open(d.join("thumbs.idx")).unwrap();
        idx.write_all(&[0xFF; 5]).unwrap();
        drop(idx);
        let store = Store::open(&d).unwrap();
        let entry = store.get(7).unwrap();
        assert_eq!(entry.facts.width, 2048);
        assert_eq!(store.read(&entry).unwrap(), (2, 2, rgba));
        assert!(store.get(8).unwrap().facts.failed);
        store.put(9, Some(&encoded), Facts::default());
        drop(store);
        let again = Store::open(&d).unwrap();
        assert!(again.get(9).is_some(), "an append after a torn tail stays aligned");
        again.reset();
        assert!(again.get(9).is_none(), "a reset forgets everything");
        again.put(10, Some(&encoded), Facts::default());
        let entry = again.get(10).unwrap();
        assert_eq!(again.read(&entry).unwrap().0, 2, "and the cache works after it");
        drop(again);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_broken_image_is_recorded_as_failed_instead_of_retried() {
        let d = dir("broken");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.png");
        std::fs::write(&f, b"not a png").unwrap();
        let lib = Library::build(d.clone(), vec![crate::library::Entry { rel: "x.png".into(), size: 9, mtime: 1 }]);
        let made = make(&f, &d, &lib.assets[0]);
        assert!(made.facts.failed && made.image.is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_visible_thumbnail_lands_in_its_tile() {
        let d = dir("land");
        std::fs::create_dir_all(d.join("lib")).unwrap();
        let f = d.join("lib/a.png");
        image::RgbaImage::new(512, 256).save(&f).unwrap();
        let len = std::fs::metadata(&f).unwrap().len();
        let lib = Arc::new(Library::build(d.join("lib"), vec![crate::library::Entry { rel: "a.png".into(), size: len, mtime: 1 }]));
        let key = lib.assets[0].key;
        let thumbs = Thumbs::new(d.join("cache"));
        thumbs.set_library(lib);
        let (pixels, fade) = thumbs.tile(key);
        assert_eq!(fade.get(), 0.0);
        thumbs.want(vec![0]);
        thumbs.settle(std::time::Duration::from_secs(5));
        assert_eq!(pixels.size(), (256, 128));
        assert_eq!(fade.get(), 1.0, "and it fades in");
        assert_eq!(thumbs.facts(key).unwrap().width, 512);
        assert_eq!(thumbs.progress(), (1, 1));
    }
}
