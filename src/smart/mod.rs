//! Search by description: an opt-in, on-device model looks at textures and models once, and
//! afterwards a typed description can find them whatever their files are called.
//!
//! Nothing here runs until the user downloads the model. Indexing is deliberately slow by default:
//! a few CPU threads, stopped on battery. The graphics card is only used if chosen, and then rests
//! twice as long as it works.

mod embedder;
mod install;
mod vectors;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use image::{Rgb, RgbImage, RgbaImage};
use rune::Waker;

use crate::library::{Asset, Kind, Library};
use crate::{model, raster};

use embedder::{Device, Embedder};
pub use install::DOWNLOAD_BYTES;
pub use vectors::Hit;
use vectors::{DIMENSIONS, Vectors};

/// Bumped when what's embedded changes meaning, so the library is indexed afresh.
const VECTORS_FILE: &str = "vectors-daa72c5-1.bin";
/// Present while the graphics card is being tried, so a crash in its driver isn't repeated.
const GPU_ATTEMPT: &str = "gpu-attempt";

/// How long the card rests for each moment it works. Each picture is a short burst at full power,
/// so what can be kept down is how much of the time is spent in one.
const GPU_REST_PER_WORK: u32 = 2;
const BATTERY_EVERY: Duration = Duration::from_secs(30);
/// Holding the search model costs memory, so it's let go between bursts of searching.
const SEARCH_MODEL_IDLE: Duration = Duration::from_secs(120);
/// Lets a word finish being typed before the model is asked about it.
const SEARCH_SETTLE: Duration = Duration::from_millis(150);
const QUERY_THREADS: usize = 4;
const MOST_HITS: usize = 4000;
/// Below this, even the best match for a search is a guess.
const LEAST_SCORE: f32 = 0.65;

const VIEW_SIZE: u32 = 256;
/// Tilt and turn of each tile of a model's contact sheet, three across and two down.
const VIEWS: [(f32, f32); 6] = [
    raster::THREE_QUARTER,
    (0.15, 0.0),
    (0.15, std::f32::consts::PI),
    (0.15, std::f32::consts::FRAC_PI_2),
    (0.15, -std::f32::consts::FRAC_PI_2),
    (1.25, 0.0),
];
/// What transparency is shown against, a middle grey being neutral to the model.
const BACKDROP: f32 = 128.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Paused,
    Cpu,
    Gpu,
    /// Everything the machine has, with no rests.
    Maximum,
}

impl Effort {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Effort {
        match code {
            0 => Effort::Paused,
            2 => Effort::Gpu,
            3 => Effort::Maximum,
            _ => Effort::Cpu,
        }
    }

    fn wants_gpu(self) -> bool {
        matches!(self, Effort::Gpu | Effort::Maximum)
    }

    /// What does the work at this effort, and how long it rests per moment of work.
    fn plan(self, gpu_broken: bool) -> (Device, u32) {
        match self {
            Effort::Maximum if gpu_broken => (Device::Cpu(all_threads()), 0),
            Effort::Maximum => (Device::Gpu, 0),
            Effort::Gpu if !gpu_broken => (Device::Gpu, GPU_REST_PER_WORK),
            _ => (Device::Cpu(gentle_threads()), 0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Install {
    Absent,
    Downloading { done: u64 },
    Failed(String),
    Ready,
    Removing,
}

/// What the settings and status line show.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub install: Install,
    pub indexed: usize,
    pub total: usize,
    /// `Some(true)` while the graphics card is working, `Some(false)` while the CPU is.
    pub working_on_gpu: Option<bool>,
    /// Why indexing isn't doing what was asked.
    pub note: Option<String>,
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    effort: Option<Effort>,
    install: Option<Install>,
    library: Option<Arc<Library>>,
    library_generation: u64,
    /// What the search box last asked, with its turn.
    asked: Option<(u64, String)>,
    answered_turn: u64,
    answer: Option<(String, Vec<Hit>)>,
    indexed: usize,
    total: usize,
    working_on_gpu: Option<bool>,
    note: Option<String>,
    gpu_broken: bool,
    index_parked: bool,
    search_parked: bool,
    workers_started: bool,
}

impl Inner {
    fn is_ready(&self) -> bool {
        self.install == Some(Install::Ready)
    }
}

struct Shared {
    dir: PathBuf,
    inner: Mutex<Inner>,
    work: Condvar,
    vectors: RwLock<Vectors>,
    downloaded: AtomicU64,
    stop_download: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn notify_app(&self) {
        if let Ok(waker) = self.waker.lock() && let Some(w) = &*waker { w(); }
    }

    fn vectors_path(&self) -> PathBuf {
        self.dir.join(VECTORS_FILE)
    }
}

#[derive(Clone)]
pub struct Smart {
    shared: Arc<Shared>,
}

impl Smart {
    pub fn new(dir: PathBuf, enabled: bool, effort: Effort) -> Smart {
        install::finish_pending_removal(&dir);
        let installed = install::is_installed(&dir);
        let vectors = if installed && enabled { Vectors::open(&dir.join(VECTORS_FILE)) } else { Vectors::default() };
        let inner = Inner {
            enabled,
            effort: Some(effort),
            install: Some(if installed { Install::Ready } else { Install::Absent }),
            ..Inner::default()
        };
        let smart = Smart { shared: Arc::new(Shared {
            dir,
            inner: Mutex::new(inner),
            work: Condvar::new(),
            vectors: RwLock::new(vectors),
            downloaded: AtomicU64::new(0),
            stop_download: AtomicBool::new(false),
            waker: Mutex::new(None),
        }) };
        smart.start_workers();
        smart
    }

    pub fn set_waker(&self, waker: Waker) {
        if let Ok(mut slot) = self.shared.waker.lock() { *slot = Some(waker); }
    }

    /// The threads only exist once there's something for them to do.
    fn start_workers(&self) {
        let mut g = self.shared.lock();
        if g.workers_started || !g.enabled || !g.is_ready() { return }
        g.workers_started = true;
        let (index, search) = (self.shared.clone(), self.shared.clone());
        let _ = std::thread::Builder::new().name("smart-index".to_owned()).spawn(move || index_loop(&index));
        let _ = std::thread::Builder::new().name("smart-search".to_owned()).spawn(move || search_loop(&search));
    }

    pub fn status(&self) -> Status {
        let g = self.shared.lock();
        let install = match g.install.clone().unwrap_or(Install::Absent) {
            Install::Downloading { .. } => Install::Downloading { done: self.shared.downloaded.load(Ordering::Relaxed) },
            other => other,
        };
        Status { install, indexed: g.indexed, total: g.total, working_on_gpu: g.working_on_gpu, note: g.note.clone() }
    }

    pub fn set_library(&self, library: Arc<Library>) {
        let mut g = self.shared.lock();
        g.library = Some(library);
        g.library_generation += 1;
        self.shared.work.notify_all();
    }

    /// Starts or resumes the download. Asking for it is what turns the feature on.
    pub fn install(&self) {
        {
            let mut g = self.shared.lock();
            if matches!(g.install, Some(Install::Downloading { .. } | Install::Ready | Install::Removing)) { return }
            g.install = Some(Install::Downloading { done: 0 });
            g.enabled = true;
        }
        self.shared.stop_download.store(false, Ordering::Relaxed);
        let this = self.clone();
        let _ = std::thread::Builder::new().name("smart-install".to_owned()).spawn(move || {
            let shared = &this.shared;
            // However this ends, the settings row must not be left saying "Downloading" forever.
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| install::run(&shared.dir, &shared.downloaded, &shared.stop_download)));
            let outcome = attempt.unwrap_or_else(|_| Err("The download stopped unexpectedly. Try again.".to_owned()));
            if matches!(outcome, Ok(true)) { *shared.vectors.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Vectors::open(&shared.vectors_path()); }
            {
                let mut g = shared.lock();
                g.install = Some(match outcome {
                    Ok(true) => Install::Ready,
                    Ok(false) => Install::Absent,
                    Err(e) => Install::Failed(e),
                });
                g.library_generation += 1;
                shared.work.notify_all();
            }
            this.start_workers();
            shared.notify_app();
        });
    }

    pub fn cancel_install(&self) {
        self.shared.stop_download.store(true, Ordering::Relaxed);
    }

    /// Stops using the model and deletes it and everything made with it.
    pub fn remove(&self) {
        {
            let mut g = self.shared.lock();
            if !matches!(g.install, Some(Install::Ready | Install::Failed(_))) { return }
            g.install = Some(Install::Removing);
            g.indexed = 0;
            g.total = 0;
            g.answer = None;
            g.asked = None;
            self.shared.work.notify_all();
        }
        let shared = self.shared.clone();
        let _ = std::thread::Builder::new().name("smart-remove".to_owned()).spawn(move || {
            // Files in use can't be deleted, so the workers let go first.
            {
                let mut g = shared.lock();
                let deadline = Instant::now() + Duration::from_secs(20);
                while g.workers_started && !(g.index_parked && g.search_parked) && Instant::now() < deadline {
                    g = shared.work.wait_timeout(g, Duration::from_millis(100)).unwrap_or_else(std::sync::PoisonError::into_inner).0;
                }
            }
            let path = shared.vectors_path();
            shared.vectors.write().unwrap_or_else(std::sync::PoisonError::into_inner).clear(&path);
            install::remove(&shared.dir);
            shared.lock().install = Some(Install::Absent);
            shared.notify_app();
        });
    }

    pub fn set_enabled(&self, enabled: bool) {
        {
            let mut g = self.shared.lock();
            g.enabled = enabled;
            if enabled && g.is_ready() && !g.workers_started {
                *self.shared.vectors.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Vectors::open(&self.shared.vectors_path());
            }
            if !enabled { g.answer = None; g.asked = None; }
            self.shared.work.notify_all();
        }
        self.start_workers();
    }

    /// Choosing the graphics card is also how a failed attempt gets another try.
    pub fn set_effort(&self, effort: Effort) {
        let mut g = self.shared.lock();
        g.effort = Some(effort);
        if effort.wants_gpu() {
            g.gpu_broken = false;
            g.note = None;
            let _ = std::fs::remove_file(self.shared.dir.join(GPU_ATTEMPT));
        }
        self.shared.work.notify_all();
    }

    /// What the search box wants found, or `None` for nothing. Repeats are ignored.
    pub fn search(&self, text: Option<String>) {
        let mut g = self.shared.lock();
        if !g.enabled || !g.is_ready() { return }
        if g.asked.as_ref().map(|(_, t)| t) == text.as_ref() { return }
        let turn = g.answered_turn.max(g.asked.as_ref().map_or(0, |(turn, _)| *turn)) + 1;
        g.answer = None;
        g.asked = text.map(|t| (turn, t));
        self.shared.work.notify_all();
    }

    pub fn take_answer(&self) -> Option<(String, Vec<Hit>)> {
        self.shared.lock().answer.take()
    }
}

/// One picture per thing worth searching for: no data maps, no other format of the same model, no
/// second copy of a texture a pack ships twice.
fn targets(library: &Library) -> Vec<u32> {
    let mut seen: HashSet<(u32, String, u64)> = HashSet::new();
    let mut out = Vec::new();
    for (i, a) in library.assets.iter().enumerate() {
        let wanted = match a.kind {
            Kind::Texture => (a.map.is_empty() || a.map == "color") && seen.insert((a.pack, a.name().to_lowercase(), a.size)),
            Kind::Model => !a.shadowed,
            Kind::Audio => false,
        };
        if wanted { out.push(i as u32); }
    }
    out
}

fn over_backdrop(rgba: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = f32::from(p[3]) / 255.0;
        let mix = |c: u8| (f32::from(c) * a + BACKDROP * (1.0 - a)) as u8;
        Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// Six views of a model on one picture, costing no more to embed than a single view and scoring
/// as well.
fn contact_sheet(model: &model::Model) -> RgbImage {
    let mut sheet = RgbImage::new(VIEW_SIZE * 3, VIEW_SIZE * 2);
    for (i, (pitch, yaw)) in VIEWS.iter().enumerate() {
        let rgba = raster::render_view(model, VIEW_SIZE, *pitch, *yaw);
        let Some(view) = RgbaImage::from_raw(VIEW_SIZE, VIEW_SIZE, rgba) else { continue };
        let (column, row) = (i as u32 % 3, i as u32 / 3);
        image::imageops::replace(&mut sheet, &over_backdrop(&view), i64::from(column * VIEW_SIZE), i64::from(row * VIEW_SIZE));
    }
    sheet
}

/// What the model is shown of `asset`, or `None` if the file can't be read.
fn picture_of(asset: &Asset, root: &Path) -> Option<RgbImage> {
    let path = root.join(&asset.rel);
    match asset.kind {
        Kind::Texture => Some(over_backdrop(&image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgba8())),
        Kind::Model => Some(contact_sheet(&model::load(&path, root).ok()?)),
        Kind::Audio => None,
    }
}

#[cfg(windows)]
fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid place for the call to write to.
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.ACLineStatus == 0
}

#[cfg(not(windows))]
fn on_battery() -> bool {
    false
}

#[cfg(windows)]
fn yield_to_everything_else() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: the pseudo-handle always refers to this thread.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

#[cfg(not(windows))]
fn yield_to_everything_else() {}

fn all_threads() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

fn gentle_threads() -> usize {
    (all_threads() / 4).clamp(1, 4)
}

fn index_loop(shared: &Shared) {
    yield_to_everything_else();
    let attempt_file = shared.dir.join(GPU_ATTEMPT);
    let mut engine: Option<(Device, Embedder)> = None;
    let mut todo: Vec<u32> = Vec::new();
    let mut library: Option<Arc<Library>> = None;
    let mut seen_generation = u64::MAX;
    let mut battery = false;
    let mut battery_checked: Option<Instant> = None;
    // A crash on the card last run left this behind.
    if attempt_file.is_file() { shared.lock().gpu_broken = true; }
    loop {
        let (device, rest_per_work) = {
            let mut g = shared.lock();
            loop {
                if battery_checked.is_none_or(|t| t.elapsed() >= BATTERY_EVERY) {
                    battery = on_battery();
                    battery_checked = Some(Instant::now());
                }
                if g.library_generation != seen_generation {
                    seen_generation = g.library_generation;
                    library = g.library.clone();
                    let all = library.as_deref().map_or_else(Vec::new, targets);
                    let lib = library.as_deref();
                    let vectors = shared.vectors.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                    todo = all.iter().rev().copied().filter(|i| lib.is_some_and(|l| !vectors.contains(l.assets[*i as usize].key))).collect();
                    g.total = all.len();
                    g.indexed = g.total - todo.len();
                }
                let effort = g.effort.unwrap_or(Effort::Cpu);
                g.note = if g.gpu_broken && effort.wants_gpu() {
                    Some("Card failed, using CPU".to_owned())
                } else if battery && effort != Effort::Paused {
                    Some("Waiting: on battery".to_owned())
                } else {
                    None
                };
                if g.is_ready() && g.enabled && effort != Effort::Paused && !battery && !todo.is_empty() {
                    g.index_parked = false;
                    break effort.plan(g.gpu_broken);
                }
                engine = None;
                g.working_on_gpu = None;
                g.index_parked = true;
                shared.work.notify_all();
                g = shared.work.wait_timeout(g, BATTERY_EVERY).unwrap_or_else(std::sync::PoisonError::into_inner).0;
            }
        };
        if engine.as_ref().is_none_or(|(d, _)| *d != device) {
            engine = None;
            if device == Device::Gpu { let _ = std::fs::write(&attempt_file, b""); }
            match Embedder::for_pictures(&shared.dir, device) {
                Ok(e) => engine = Some((device, e)),
                Err(e) if device == Device::Gpu => {
                    eprintln!("graphics card unavailable for indexing: {e}");
                    shared.lock().gpu_broken = true;
                    continue;
                }
                Err(e) => {
                    eprintln!("couldn't start indexing: {e}");
                    let mut g = shared.lock();
                    g.note = Some("Couldn't start indexing".to_owned());
                    g.index_parked = true;
                    shared.work.notify_all();
                    let _ = shared.work.wait_timeout(g, Duration::from_secs(60));
                    continue;
                }
            }
            shared.lock().working_on_gpu = Some(device == Device::Gpu);
        }
        let (Some(id), Some(lib), Some((_, embedder))) = (todo.pop(), library.as_deref(), engine.as_mut()) else { continue };
        let asset = &lib.assets[id as usize];
        let picture = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| picture_of(asset, &lib.root))).ok().flatten();
        let started = Instant::now();
        let vector = match picture.map(|p| embedder.picture(&p)) {
            Some(Ok(v)) => v,
            Some(Err(e)) if device == Device::Gpu => {
                eprintln!("graphics card failed while indexing: {e}");
                todo.push(id);
                shared.lock().gpu_broken = true;
                continue;
            }
            Some(Err(_)) | None => vec![0.0; DIMENSIONS],
        };
        let worked = started.elapsed();
        if device == Device::Gpu { let _ = std::fs::remove_file(&attempt_file); }
        shared.vectors.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(asset.key, &vector);
        shared.lock().indexed += 1;
        if rest_per_work > 0 { std::thread::sleep(worked * rest_per_work); }
    }
}

fn search_loop(shared: &Shared) {
    let mut engine: Option<Embedder> = None;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, QUERY_THREADS);
    loop {
        let (turn, text) = {
            let mut g = shared.lock();
            loop {
                let asked = g.asked.clone().filter(|(turn, _)| *turn > g.answered_turn);
                if let Some(job) = asked.filter(|_| g.is_ready() && g.enabled) {
                    g.search_parked = false;
                    break job;
                }
                g.search_parked = true;
                if !g.is_ready() || !g.enabled { engine = None; }
                shared.work.notify_all();
                let (next, waited) = shared.work.wait_timeout(g, SEARCH_MODEL_IDLE).unwrap_or_else(std::sync::PoisonError::into_inner);
                g = next;
                if waited.timed_out() { engine = None; }
            }
        };
        std::thread::sleep(SEARCH_SETTLE);
        if shared.lock().asked.as_ref().is_none_or(|(t, _)| *t != turn) { continue }
        if engine.is_none() {
            match Embedder::for_queries(&shared.dir, Device::Cpu(threads)) {
                Ok(e) => engine = Some(e),
                Err(e) => {
                    eprintln!("search model unavailable: {e}");
                    shared.lock().answered_turn = turn;
                    continue;
                }
            }
        }
        let Some(embedder) = engine.as_mut() else { continue };
        let hits = embedder.query(&text).map(|q| shared.vectors.read().unwrap_or_else(std::sync::PoisonError::into_inner).nearest(&q, LEAST_SCORE, MOST_HITS));
        let mut g = shared.lock();
        g.answered_turn = turn;
        match hits {
            Ok(hits) if g.asked.as_ref().is_some_and(|(t, _)| *t == turn) => g.answer = Some((text, hits)),
            Ok(_) => {}
            Err(e) => eprintln!("search failed: {e}"),
        }
        drop(g);
        shared.notify_app();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Entry;

    fn library(files: &[(&str, u64)]) -> Library {
        Library::build(PathBuf::from("Z:/none"), files.iter().map(|(rel, size)| Entry { rel: (*rel).to_owned(), size: *size, mtime: 1 }).collect())
    }

    fn picked(lib: &Library) -> Vec<&str> {
        targets(lib).iter().map(|i| lib.assets[*i as usize].rel.as_str()).collect()
    }

    #[test]
    fn data_maps_and_sounds_are_left_out_and_colour_maps_kept() {
        let lib = library(&[("P/wall_basecolor.png", 1), ("P/wall_normal.png", 1), ("P/wall_roughness.png", 1), ("P/plain.png", 1), ("P/hit.wav", 1)]);
        assert_eq!(picked(&lib), ["P/plain.png", "P/wall_basecolor.png"]);
    }

    #[test]
    fn a_model_is_embedded_once_whatever_formats_it_ships_in() {
        let lib = library(&[("P/GLB/door.glb", 1), ("P/FBX/door.fbx", 1), ("P/DAE/door.dae", 1), ("P/GLB/chair.glb", 1)]);
        assert_eq!(picked(&lib), ["P/GLB/chair.glb", "P/GLB/door.glb"]);
    }

    #[test]
    fn a_texture_copied_around_a_pack_is_embedded_once_but_lookalikes_are_not_merged() {
        let lib = library(&[("P/A/door.png", 9), ("P/B/door.png", 9), ("P/C/door.png", 10), ("Q/door.png", 9)]);
        assert_eq!(picked(&lib).len(), 3);
    }

    #[test]
    fn efforts_survive_being_saved() {
        for e in [Effort::Paused, Effort::Cpu, Effort::Gpu, Effort::Maximum] { assert_eq!(Effort::from_code(e.code()), e); }
    }

    #[test]
    fn only_maximum_runs_without_rests_or_a_thread_limit() {
        assert_eq!(Effort::Gpu.plan(false), (Device::Gpu, GPU_REST_PER_WORK));
        assert_eq!(Effort::Maximum.plan(false), (Device::Gpu, 0));
        assert_eq!(Effort::Cpu.plan(false), (Device::Cpu(gentle_threads()), 0));
    }

    #[test]
    fn a_broken_graphics_card_falls_back_to_the_cpu_at_the_level_asked_for() {
        assert_eq!(Effort::Gpu.plan(true), (Device::Cpu(gentle_threads()), 0));
        assert_eq!(Effort::Maximum.plan(true), (Device::Cpu(all_threads()), 0));
    }

    #[test]
    fn indexing_uses_only_a_few_threads_on_any_machine() {
        assert!((1..=4).contains(&gentle_threads()));
    }

    #[test]
    fn a_contact_sheet_shows_six_views_over_grey() {
        let v = |x: f32, y: f32| model::Vertex { pos: [x, y, 0.0], normal: [0.0, 0.0, 1.0], uv: [0.0; 2] };
        let m = model::Model {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            parts: vec![model::Part { start: 0, count: 3, material: 0 }],
            materials: vec![model::Material { color: [1.0, 0.0, 0.0, 1.0], image: None }],
            ..Default::default()
        };
        let sheet = contact_sheet(&m);
        assert_eq!(sheet.dimensions(), (768, 512));
        assert_eq!(sheet.get_pixel(0, 0).0, [128, 128, 128], "the backdrop shows where the model doesn't");
        let red = sheet.pixels().filter(|p| p.0[0] > 200 && p.0[1] < 60).count();
        assert!(red > 100, "the model was drawn: {red}");
    }

    /// Downloads for real unless `AB_MODEL_DIR` already holds the model. `AB_SMART_LIBRARY` is a
    /// small folder of assets, `AB_SMART_QUERY` a description something in it matches, and
    /// `AB_SMART_EFFORT` is `cpu`, `gpu` or `maximum`.
    #[test]
    #[ignore = "downloads the model and runs it"]
    fn download_index_search_and_remove_through_the_real_workers() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
        let dir = PathBuf::from(var("AB_MODEL_DIR"));
        let root = PathBuf::from(var("AB_SMART_LIBRARY"));
        let effort = match var("AB_SMART_EFFORT").as_str() {
            "gpu" => Effort::Gpu,
            "maximum" => Effort::Maximum,
            _ => Effort::Cpu,
        };
        let wait_for = |what: &str, limit: Duration, done: &dyn Fn() -> bool| {
            let started = Instant::now();
            while !done() {
                assert!(started.elapsed() < limit, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(200));
            }
            println!("{what}: {:.1}s", started.elapsed().as_secs_f32());
        };

        let smart = Smart::new(dir.clone(), true, effort);
        smart.install();
        wait_for("install", Duration::from_secs(3000), &|| matches!(smart.status().install, Install::Ready | Install::Failed(_)));
        assert_eq!(smart.status().install, Install::Ready);

        let library = Arc::new(Library::build(root.clone(), crate::library::walk(&root)));
        let started = Instant::now();
        smart.set_library(library.clone());
        wait_for("targets", Duration::from_secs(10), &|| smart.status().total > 0);
        wait_for("indexing", Duration::from_secs(1500), &|| { let s = smart.status(); s.indexed == s.total });
        let status = smart.status();
        println!("{} items, {:.2}s each, note {:?}", status.total, started.elapsed().as_secs_f32() / status.total as f32, status.note);

        // A new run reads what the last one stored and has nothing left to do.
        let again = Smart::new(dir.clone(), true, Effort::Paused);
        again.set_library(library.clone());
        wait_for("reading the index back", Duration::from_secs(10), &|| again.status().total == status.total);
        assert_eq!(again.status().indexed, status.total, "nothing is indexed twice");

        let query = var("AB_SMART_QUERY");
        smart.search(Some(query.clone()));
        let answer = std::cell::RefCell::new(None);
        wait_for("the first search", Duration::from_secs(30), &|| { if let Some(a) = smart.take_answer() { *answer.borrow_mut() = Some(a); } answer.borrow().is_some() });
        let (asked, hits) = answer.into_inner().unwrap();
        assert_eq!(asked, query);
        assert!(!hits.is_empty(), "something in the folder matches");
        for h in hits.iter().take(5) {
            println!("{:.3} {}", h.score, library.assets.iter().find(|a| a.key == h.key).map_or("?", |a| a.rel.as_str()));
        }
        drop(again);
        smart.set_effort(Effort::Paused);
        smart.remove();
        wait_for("removal", Duration::from_secs(30), &|| smart.status().install == Install::Absent);
        assert!(!install::is_installed(&dir));
    }

    #[test]
    fn nothing_starts_until_the_user_asks_for_it() {
        let dir = std::env::temp_dir().join(format!("ab-smart-off-{}", std::process::id()));
        let smart = Smart::new(dir.clone(), false, Effort::Cpu);
        assert_eq!(smart.status().install, Install::Absent);
        smart.search(Some("a door".to_owned()));
        assert!(smart.take_answer().is_none());
        assert!(!dir.exists(), "not even a folder");
    }
}
