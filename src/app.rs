//! The application's state and everything that changes it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use rune::prefs::Prefs;
use rune::{Latest, Live, LiveText, Pixels, ScrollPos, Spring, TextEdit, Tx, Waker, WheelEvent};

use crate::audio::Audio;
use crate::export::{self, Options};
use crate::library::{self, Filter, Kind, Library, Results, Sort};
use crate::model::{self, Image, Model};
use crate::native_drag::{self, Ended};
use crate::stage::{Scene, StageState, View};
use crate::thumbs::{Thumbs, WAVE_COLOR};
use crate::{ui, wave};

const DEFAULT_ROOT: &str = r"C:\Users\cuddl\Documents\Dev\Games\Assets";
pub const WAVE_W: u32 = 960;
pub const WAVE_H: u32 = 480;

/// Rows beyond the viewport, each way, whose thumbnails are fetched ahead of arriving.
const LOOKAHEAD_ROWS: usize = 12;
/// What's fetched before the grid has reported its visible rows.
const FIRST_SCREEN_ROWS: usize = 8;

pub const INSPECTOR_DEFAULT: f32 = 372.0;
const INSPECTOR_MIN: f32 = 300.0;
const INSPECTOR_MAX: f32 = 900.0;
/// The grid keeps at least this much when the inspector grows.
const MAIN_MIN_WIDTH: f32 = 600.0;
const INSPECTOR_PREF: &str = "inspector_width";
const SIDEBAR_PREF: &str = "sidebar_open";
const SIDEBAR_SPRING: Spring = Spring::new(0.3, 0.0);
/// How long a button shows that its action happened.
const CONFIRM_FOR: Duration = Duration::from_millis(1400);

pub enum Preview {
    None,
    Loading,
    Texture(Arc<Image>),
    Model(Arc<Model>),
    Audio { seconds: f32, rate: u32, channels: u16 },
    Failed(String),
}

enum Loaded {
    Texture(Arc<Image>),
    Model(Arc<Model>),
    Audio { rgba: Vec<u8>, seconds: f32, rate: u32, channels: u16 },
    Failed(String),
}

/// Eases a notched mouse wheel: each step moves a target, and the view springs to it. Trackpads
/// already deliver smooth deltas and are left alone.
#[derive(Clone, Default)]
pub struct Glide {
    /// The viewport's offset and how far it can scroll.
    position: Rc<Cell<[f32; 2]>>,
    target: Rc<Cell<Option<(f32, Instant)>>>,
}

/// A wheel notch arrives as 40 points; this makes one notch a useful distance.
const WHEEL_GAIN: f32 = 2.5;
const WHEEL_SPRING: Spring = Spring::new(0.2, 0.0);
/// A target older than this is finished with: the view may have been moved another way since.
const GLIDE_MEMORY: Duration = Duration::from_millis(350);

impl Glide {
    /// What the viewport's `on_scroll_offset` should report to.
    pub fn watch(&self) -> impl Fn(ScrollPos) + 'static {
        let position = self.position.clone();
        move |p: ScrollPos| position.set([p.offset[1], p.max[1]])
    }

    /// Takes a wheel event over viewport `key`; false leaves it to the viewport.
    pub fn wheel(&self, key: &str, e: WheelEvent, tx: &mut Tx) -> bool {
        if e.precise || e.dy == 0.0 { return false }
        let [offset, max] = self.position.get();
        let from = self.target.get().filter(|(_, at)| at.elapsed() < GLIDE_MEMORY).map_or(offset, |(y, _)| y);
        let to = (from - e.dy * WHEEL_GAIN).clamp(0.0, max);
        self.target.set(Some((to, Instant::now())));
        tx.scroll_to_with(key, 0.0, to, WHEEL_SPRING);
        tx.live_only();
        true
    }

    pub fn forget(&self) {
        self.target.set(None);
    }
}

pub struct App {
    pub lib: Arc<Library>,
    pub results: Results,
    /// Bumped whenever `results` is replaced, so rows know to rebuild.
    pub generation: u64,
    pub filter: Filter,
    pub search: TextEdit,
    pub thumbs: Thumbs,
    pub audio: Audio,
    pub scanning: bool,
    pub selected: Option<u32>,
    pub preview: Preview,
    awaiting_preview: bool,
    loading: Latest<Loaded>,
    pub stage: StageState,
    /// The stage's size in points.
    pub stage_size: [f32; 2],
    pub scale: f32,
    pub drag_last: [f32; 2],
    pub wave: Pixels,
    pub playhead: Live<[f32; 2]>,
    pub clock: LiveText,
    pub autoplay: bool,
    pub grid_width: f32,
    pub window_width: f32,
    pub window_height: f32,
    pub sidebar_open: bool,
    pub grid_glide: Glide,
    pub packs_glide: Glide,
    /// Which button just confirmed an action, and when that confirmation ends.
    pub confirmed: Option<(&'static str, Instant)>,
    /// Whether the audio was playing when the view last ran.
    pub showing_playing: bool,
    /// Keeps checking playback while a sound is still opening.
    audio_watch_until: Option<Instant>,
    inspector_w: f32,
    prefs: Prefs,
    pub columns: usize,
    pub tile_size: f32,
    /// The largest side textures are dragged out of the grid at.
    pub drag_max: Option<u32>,
    pub nearest: bool,
    pub companions: bool,
    pub status: LiveText,
    /// Why the last drag didn't work, or what it staged.
    pub message: Option<String>,
    drag_started: bool,
    drag_results: (Sender<Ended>, Receiver<Ended>),
    drag_label: String,
    /// Files the last drag staged, for headless checks.
    pub last_staged: Vec<PathBuf>,
    pub dragging: Option<u32>,
    root: PathBuf,
    loads: (Sender<LibraryMessage>, Receiver<LibraryMessage>),
    waker: Option<Waker>,
    scene: Option<&'static str>,
}

enum LibraryMessage {
    Ready(Library, bool),
    ScanDone,
}

fn snapshot_path() -> PathBuf {
    export::app_dir().join("library.tsv")
}

fn root_setting() -> PathBuf {
    export::app_dir().join("root.txt")
}

fn configured_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ASSET_BROWSER_ROOT") { return root.into(); }
    std::fs::read_to_string(root_setting()).ok().map(|t| PathBuf::from(t.trim())).filter(|p| p.is_dir()).unwrap_or_else(|| DEFAULT_ROOT.into())
}

pub fn init() -> App {
    let prefs = Prefs::open("asset-browser-v2");
    let inspector_w = prefs.get(INSPECTOR_PREF).unwrap_or(INSPECTOR_DEFAULT);
    let sidebar_open = prefs.get(SIDEBAR_PREF).unwrap_or(true);
    App {
        lib: Arc::new(Library::default()),
        results: Results::default(),
        generation: 1,
        filter: Filter::default(),
        search: TextEdit::new(""),
        thumbs: Thumbs::new(export::app_dir().join("cache")),
        audio: Audio::new(),
        scanning: true,
        selected: None,
        preview: Preview::None,
        awaiting_preview: false,
        loading: Latest::default(),
        stage: StageState::default(),
        stage_size: [320.0, 320.0],
        scale: 1.0,
        drag_last: [0.0; 2],
        wave: Pixels::new(),
        playhead: Live::new([0.0, 0.0]),
        clock: LiveText::new("0:00.0"),
        autoplay: true,
        grid_width: 900.0,
        window_width: 1520.0,
        window_height: 900.0,
        sidebar_open,
        grid_glide: Glide::default(),
        packs_glide: Glide::default(),
        confirmed: None,
        showing_playing: false,
        audio_watch_until: None,
        inspector_w,
        prefs,
        columns: 5,
        tile_size: 158.0,
        drag_max: None,
        nearest: false,
        companions: true,
        status: LiveText::new("Reading library"),
        message: None,
        drag_started: false,
        drag_results: channel(),
        drag_label: String::new(),
        last_staged: Vec::new(),
        dragging: None,
        root: configured_root(),
        loads: channel(),
        waker: None,
        scene: None,
    }
}

pub fn setup(s: &mut App, waker: Waker) {
    s.loading.set_waker(waker.clone());
    s.waker = Some(waker);
    let root = s.root.clone();
    s.start_loading(root);
    std::thread::spawn(export::prune);
}

fn headless() -> bool {
    rune::tooling::headless_run().is_some()
}

impl App {
    fn wake(&self) {
        if let Some(w) = &self.waker { w(); }
    }

    /// Shows the snapshot from last time at once, then walks the disk for what changed.
    fn start_loading(&mut self, root: PathBuf) {
        self.root = root.clone();
        self.scanning = true;
        let tx = self.loads.0.clone();
        let waker = self.waker.clone();
        let work = move || {
            let notify = || if let Some(w) = &waker { w(); };
            let snapshot = library::load_snapshot(&snapshot_path(), &root);
            let known = snapshot.as_ref().map(|l| l.assets.iter().map(|a| a.key).collect::<Vec<_>>());
            if let Some(lib) = snapshot {
                let _ = tx.send(LibraryMessage::Ready(lib, true));
                notify();
                // A headless run reads the snapshot and stops: output doesn't race the disk.
                if headless() {
                    let _ = tx.send(LibraryMessage::ScanDone);
                    notify();
                    return;
                }
            }
            let fresh = Library::build(root.clone(), library::walk(&root));
            let changed = known.is_none_or(|k| k != fresh.assets.iter().map(|a| a.key).collect::<Vec<_>>());
            if changed {
                let _ = library::save_snapshot(&snapshot_path(), &fresh);
                let _ = tx.send(LibraryMessage::Ready(fresh, false));
            }
            let _ = tx.send(LibraryMessage::ScanDone);
            notify();
        };
        if headless() { work(); } else { std::thread::spawn(work); }
    }

    pub fn apply_scene(&mut self, name: &'static str) {
        self.scene = Some(name);
    }

    fn run_scene(&mut self, name: &str, tx: &mut Tx) {
        let first = |s: &App, kind: Kind, hint: &str| {
            s.lib.assets.iter().position(|a| a.kind == kind && !a.shadowed && a.hay.contains(hint)).map(|i| i as u32)
        };
        match name {
            "textures" => self.set_kind(Some(Kind::Texture), tx),
            "models" => self.set_kind(Some(Kind::Model), tx),
            "audio" => self.set_kind(Some(Kind::Audio), tx),
            "search" => {
                self.search = TextEdit::new("rust metal");
                self.refilter(tx);
            }
            "small" => self.tile_size = 120.0,
            "texture" | "tile" | "tile-seams" => {
                self.set_kind(Some(Kind::Texture), tx);
                if let Some(id) = first(self, Kind::Texture, "brick").or_else(|| first(self, Kind::Texture, "")) {
                    self.select(id, tx);
                }
                if name != "texture" { self.set_tile(true); }
                self.stage.view.seams = name == "tile-seams";
            }
            "big-texture" => {
                self.set_kind(Some(Kind::Texture), tx);
                if let Some(id) = first(self, Kind::Texture, "1024x1024") { self.select(id, tx); }
            }
            "model" => {
                self.set_kind(Some(Kind::Model), tx);
                if let Some(id) = first(self, Kind::Model, "barrel").or_else(|| first(self, Kind::Model, "")) {
                    self.select(id, tx);
                }
            }
            "sound" => {
                self.set_kind(Some(Kind::Audio), tx);
                if let Some(id) = first(self, Kind::Audio, "") { self.select(id, tx); }
            }
            _ => {}
        }
    }

    pub fn poll(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        if headless() { self.finish_preview_blocking(); }
        while let Ok(message) = self.loads.1.try_recv() {
            match message {
                LibraryMessage::Ready(lib, from_snapshot) => {
                    self.adopt(lib);
                    tx.instant();
                    if from_snapshot { self.scanning = true; }
                    changed = true;
                }
                LibraryMessage::ScanDone => {
                    self.scanning = false;
                    if self.lib.assets.is_empty() { changed = true; }
                }
            }
        }
        if let Some(name) = self.scene.take().filter(|_| !self.lib.assets.is_empty()) {
            self.run_scene(name, tx);
            changed = true;
        }
        if let Some(loaded) = self.loading.take() {
            self.land_preview(loaded);
            changed = true;
        }
        if let Ok(ended) = self.drag_results.1.try_recv() {
            self.message = Some(match ended {
                Ended::Dropped => format!("Copied {}", self.drag_label),
                Ended::Cancelled => "Drag cancelled".to_owned(),
                Ended::Failed => format!("Couldn't start a drag for {}", self.drag_label),
            });
            self.dragging = None;
            changed = true;
        }
        self.update_status(tx);
        if self.confirmed.is_some_and(|(_, until)| Instant::now() >= until) {
            self.confirmed = None;
            changed = true;
        }
        if matches!(self.preview, Preview::Audio { .. }) { self.track_playback(tx); }
        let playing = self.audio.is_playing();
        if playing != self.showing_playing {
            self.showing_playing = playing;
            changed = true;
        }
        // Snapshots wait for their thumbnails. `AB_FRAME_MS` runs a benchmark at that pace
        // against the real workers instead.
        if headless() {
            match std::env::var("AB_FRAME_MS").ok().and_then(|v| v.parse().ok()) {
                Some(ms) => std::thread::sleep(Duration::from_millis(ms)),
                None if self.thumbs.busy() => self.thumbs.settle(Duration::from_secs(20)),
                None => {}
            }
        }
        changed
    }

    fn update_status(&mut self, tx: &mut Tx) {
        let (made, total) = self.thumbs.progress();
        let text = if total == 0 {
            if self.scanning { "Reading library".to_owned() } else { "No supported files in this folder".to_owned() }
        } else if made < total {
            format!("Making thumbnails: {} of {}", commas(made), commas(total))
        } else if self.scanning {
            "Checking for changes".to_owned()
        } else {
            "Drag assets out to copy them. Your library stays untouched.".to_owned()
        };
        self.status.set(text);
        if self.scanning || made < total { tx.wake_after(Duration::from_millis(400)); }
    }

    fn track_playback(&mut self, tx: &mut Tx) {
        let Preview::Audio { seconds, .. } = self.preview else { return };
        let position = self.audio.position().unwrap_or(0.0).min(seconds);
        let x = position / seconds.max(0.001) * self.stage_size[0];
        self.playhead.set([x, 0.0]);
        self.clock.set(format!("{} / {}", clock(position), clock(seconds)));
        let opening = self.audio_watch_until.is_some_and(|t| Instant::now() < t);
        if self.audio.is_playing() || opening { tx.wake_after(Duration::from_millis(33)); }
    }

    /// Shows on button `name` that its action happened.
    pub fn confirm(&mut self, name: &'static str, tx: &mut Tx) {
        self.confirmed = Some((name, Instant::now() + CONFIRM_FOR));
        tx.wake_after(CONFIRM_FOR);
    }

    pub fn is_confirmed(&self, name: &str) -> bool {
        self.confirmed.is_some_and(|(n, _)| n == name)
    }

    pub fn toggle_sidebar(&mut self, tx: &mut Tx) {
        self.sidebar_open = !self.sidebar_open;
        tx.motion(SIDEBAR_SPRING);
        if !headless() { let _ = self.prefs.set(SIDEBAR_PREF, &self.sidebar_open); }
    }

    fn adopt(&mut self, lib: Library) {
        let selected = self.selected.and_then(|i| self.lib.assets.get(i as usize)).map(|a| a.rel.clone());
        // Filters hold pack numbers, which a new index can renumber.
        let pack = self.filter.pack.and_then(|p| self.lib.packs.get(p as usize)).map(|p| p.name.clone());
        let lib = Arc::new(lib);
        self.lib = lib.clone();
        self.filter.pack = pack.and_then(|name| lib.packs.iter().position(|p| p.name == name)).map(|p| p as u32);
        self.selected = selected.and_then(|rel| lib.assets.iter().position(|a| a.rel == rel)).map(|i| i as u32);
        self.thumbs.set_library(lib);
        self.refilter_keeping_scroll();
    }

    fn refilter_keeping_scroll(&mut self) {
        self.filter.text = self.search.text.clone();
        self.results = library::search(&self.lib, &self.filter);
        self.generation += 1;
        self.request_range(0..FIRST_SCREEN_ROWS);
    }

    pub fn refilter(&mut self, tx: &mut Tx) {
        self.refilter_keeping_scroll();
        self.grid_glide.forget();
        tx.scroll_to_instant("grid", 0.0, 0.0);
    }

    pub fn set_kind(&mut self, kind: Option<Kind>, tx: &mut Tx) {
        self.filter.kind = kind;
        self.filter.ext = None;
        self.refilter(tx);
    }

    pub fn set_pack(&mut self, pack: Option<u32>, tx: &mut Tx) {
        self.filter.pack = pack;
        self.filter.ext = None;
        self.refilter(tx);
    }

    pub fn set_ext(&mut self, ext: Option<&'static str>, tx: &mut Tx) {
        self.filter.ext = if self.filter.ext == ext { None } else { ext };
        self.refilter(tx);
    }

    pub fn set_sort(&mut self, sort: Sort, tx: &mut Tx) {
        self.filter.sort = sort;
        self.refilter(tx);
    }

    pub fn toggle_all_formats(&mut self, tx: &mut Tx) {
        self.filter.all_formats = !self.filter.all_formats;
        self.refilter(tx);
    }

    pub fn set_grid_width(&mut self, width: f32) {
        self.grid_width = width;
        self.columns = self.columns_for(width);
    }

    fn columns_for(&self, width: f32) -> usize {
        ((width - GRID_INSET * 2.0 + GRID_GAP) / (self.tile_size + GRID_GAP)).floor().max(1.0) as usize
    }

    pub fn set_tile_size(&mut self, size: f32) {
        self.tile_size = size;
        self.columns = self.columns_for(self.grid_width);
        self.generation += 1;
    }

    pub fn row_count(&self) -> usize {
        self.results.ids.len().div_ceil(self.columns.max(1))
    }

    pub fn tile_width(&self) -> f32 {
        let cols = self.columns.max(1) as f32;
        ((self.grid_width - GRID_INSET * 2.0 - GRID_GAP * (cols - 1.0)) / cols).floor()
    }

    /// Fetches what's on screen first, then outward.
    pub fn request_range(&self, rows: std::ops::Range<usize>) {
        let cols = self.columns.max(1);
        let ids = &self.results.ids;
        let total_rows = self.row_count();
        let mut want = Vec::with_capacity((rows.len() + LOOKAHEAD_ROWS * 2) * cols);
        let mut push_row = |r: usize| {
            for i in r * cols..((r + 1) * cols).min(ids.len()) { want.push(ids[i]); }
        };
        for r in rows.clone() { push_row(r); }
        for step in 0..LOOKAHEAD_ROWS {
            if rows.end + step < total_rows { push_row(rows.end + step); }
            if rows.start > step { push_row(rows.start - step - 1); }
        }
        self.thumbs.want(want);
    }

    pub fn select(&mut self, id: u32, tx: &mut Tx) {
        if self.selected == Some(id) { return }
        self.selected = Some(id);
        let Some(asset) = self.lib.assets.get(id as usize).cloned() else { return };
        let path = self.lib.path(&asset);
        self.message = None;
        // The last preview stays up until this one lands, so stepping through assets doesn't
        // blank the panel between each.
        if matches!(self.preview, Preview::None) { self.preview = Preview::Loading; }
        self.awaiting_preview = true;
        self.audio.stop();
        self.loading.request((path.clone(), asset.kind), move |(p, kind)| load_preview(&p, kind));
        if asset.kind == Kind::Audio && self.autoplay && !headless() {
            self.audio.play(path);
            self.watch_audio(tx);
        }
        tx.wake_after(Duration::from_millis(1));
    }

    fn finish_preview_blocking(&mut self) {
        let start = std::time::Instant::now();
        while self.awaiting_preview && self.loading.is_pending() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Whether the selected asset's preview has arrived.
    #[cfg(test)]
    pub fn preview_landed(&self) -> bool {
        !self.awaiting_preview
    }

    fn land_preview(&mut self, loaded: Loaded) {
        self.awaiting_preview = false;
        let keep = self.stage.view;
        self.stage.view = View { tile: keep.tile, seams: keep.seams, smooth: keep.smooth, zoom: if keep.tile { 0.34 } else { 1.0 }, ..View::default() };
        self.playhead.set([0.0, 0.0]);
        self.preview = match loaded {
            Loaded::Texture(img) => {
                self.stage.scene = Scene::Texture(img.clone());
                self.stage.revision += 1;
                Preview::Texture(img)
            }
            Loaded::Model(m) => {
                self.stage.scene = Scene::Model(m.clone());
                self.stage.revision += 1;
                Preview::Model(m)
            }
            Loaded::Audio { rgba, seconds, rate, channels } => {
                self.wave.set(WAVE_W, WAVE_H, rgba);
                Preview::Audio { seconds, rate, channels }
            }
            Loaded::Failed(reason) => Preview::Failed(reason),
        };
        self.stage.snap += 1;
    }

    /// The inspector's width after the window has had its say.
    pub fn inspector_width(&self) -> f32 {
        let sidebar = if self.sidebar_open { ui::SIDEBAR_W + ui::WINDOW_GAP } else { 0.0 };
        let room = self.window_width - sidebar - ui::WINDOW_GAP * 3.0 - MAIN_MIN_WIDTH;
        self.inspector_w.min(room).max(INSPECTOR_MIN)
    }

    pub fn resize_inspector(&mut self, width: f32) {
        self.inspector_w = width.round().clamp(INSPECTOR_MIN, INSPECTOR_MAX);
    }

    pub fn save_inspector_width(&mut self) {
        if !headless() { let _ = self.prefs.set(INSPECTOR_PREF, &self.inspector_w); }
    }

    pub fn set_tile(&mut self, on: bool) {
        self.stage.view.tile = on;
        self.stage.view.zoom = if on { 0.34 } else { 1.0 };
        self.stage.view.offset = [0.0; 2];
    }

    pub fn reset_view(&mut self) {
        let keep = self.stage.view;
        self.stage.view = View { tile: keep.tile, seams: keep.seams, smooth: keep.smooth, zoom: if keep.tile { 0.34 } else { 1.0 }, ..View::default() };
    }

    pub fn toggle_playback(&mut self, tx: &mut Tx) {
        let Some(asset) = self.selected.and_then(|i| self.lib.assets.get(i as usize)) else { return };
        if asset.kind != Kind::Audio { return }
        if self.audio.is_finished() {
            self.audio.play(self.lib.path(asset));
        } else {
            self.audio.toggle();
        }
        self.watch_audio(tx);
    }

    fn watch_audio(&mut self, tx: &mut Tx) {
        self.audio_watch_until = Some(Instant::now() + Duration::from_millis(600));
        tx.wake_after(Duration::from_millis(33));
    }

    pub fn seek_fraction(&mut self, fraction: f32, tx: &mut Tx) {
        let Preview::Audio { seconds, .. } = self.preview else { return };
        let Some(asset) = self.selected.and_then(|i| self.lib.assets.get(i as usize)) else { return };
        let at = fraction.clamp(0.0, 1.0) * seconds;
        if self.audio.is_finished() { self.audio.play(self.lib.path(asset)); }
        self.audio.seek(at);
        self.playhead.set([fraction.clamp(0.0, 1.0) * self.stage_size[0], 0.0]);
        tx.wake_after(Duration::from_millis(33));
    }

    /// Selection moved by `by` tiles, kept in view.
    pub fn step_selection(&mut self, by: isize, tx: &mut Tx) {
        let ids = &self.results.ids;
        if ids.is_empty() { return }
        let at = self.selected.and_then(|sel| ids.iter().position(|i| *i == sel)).map_or(0, |p| {
            (p as isize + by).clamp(0, ids.len() as isize - 1) as usize
        });
        let id = ids[at];
        let row = at / self.columns.max(1);
        self.select(id, tx);
        tx.scroll_into_view(format!("grid/rows/r{row}"));
    }

    pub fn options(&self, max_size: Option<u32>) -> Options {
        Options { max_size, nearest: self.nearest, companions: self.companions }
    }

    /// Starts copying a big file the moment it's pressed, so the drag doesn't wait for it.
    pub fn prestage(&self, id: u32, options: Options) {
        let Some(asset) = self.lib.assets.get(id as usize).cloned() else { return };
        if asset.size < 16 * 1024 * 1024 { return }
        let path = self.lib.path(&asset);
        std::thread::spawn(move || { let _ = export::stage(&path, &asset, options); });
    }

    /// The drag gesture on a tile or a size rung.
    pub fn drag_gesture(&mut self, id: u32, max_size: Option<u32>, drag: &rune::Drag) {
        use rune::Phase;
        let options = self.options(max_size);
        match drag.phase {
            Phase::Down => {
                self.drag_started = false;
                self.prestage(id, options);
            }
            Phase::Move if !self.drag_started && drag.dx.hypot(drag.dy) > 5.0 => {
                self.drag_started = true;
                self.start_drag(id, options);
            }
            Phase::Up => self.drag_started = false,
            Phase::Move => {}
        }
    }

    fn start_drag(&mut self, id: u32, options: Options) {
        let Some(asset) = self.lib.assets.get(id as usize).cloned() else { return };
        let path = self.lib.path(&asset);
        let files = match export::stage(&path, &asset, options) {
            Ok(files) => files,
            Err(reason) => {
                self.message = Some(format!("Couldn't prepare {}: {reason}", asset.name()));
                return;
            }
        };
        self.last_staged = files.clone();
        self.drag_label = files.first().and_then(|f| f.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if headless() {
            self.message = Some(format!("Staged {}", self.drag_label));
            return;
        }
        self.dragging = Some(id);
        let image = self.thumbs.image(asset.key).map(drag_picture);
        let done = self.drag_results.0.clone();
        native_drag::begin(native_drag::Request { files, image, done });
    }

    pub fn change_root(&mut self, root: PathBuf) {
        let _ = std::fs::create_dir_all(export::app_dir());
        let _ = std::fs::write(root_setting(), root.to_string_lossy().as_bytes());
        self.selected = None;
        self.preview = Preview::None;
        self.audio.stop();
        self.start_loading(root);
        self.wake();
    }

    pub fn reveal(&self, id: u32) {
        let Some(asset) = self.lib.assets.get(id as usize) else { return };
        reveal_in_explorer(&self.lib.path(asset));
    }

    pub fn asset_path(&self, id: u32) -> Option<PathBuf> {
        self.lib.assets.get(id as usize).map(|a| self.lib.path(a))
    }
}

pub const GRID_PAD: f32 = 16.0;
pub const TILE_PAD: f32 = 4.0;
/// Tiles carry their own padding, so the grid sits this far in for pictures to meet `GRID_PAD`.
pub const GRID_INSET: f32 = GRID_PAD - TILE_PAD;
pub const GRID_GAP: f32 = 4.0;

#[cfg(windows)]
fn reveal_in_explorer(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

#[cfg(not(windows))]
fn reveal_in_explorer(_: &Path) {}

/// Fits a thumbnail into the 128-point picture under the pointer.
fn drag_picture((w, h, rgba): (u32, u32, Vec<u8>)) -> (u32, u32, Vec<u8>) {
    let img = image::RgbaImage::from_raw(w, h, rgba).unwrap_or_default();
    let fitted = image::DynamicImage::ImageRgba8(img).thumbnail(128, 128).to_rgba8();
    let (fw, fh) = fitted.dimensions();
    (fw, fh, fitted.into_raw())
}

fn load_preview(path: &Path, kind: Kind) -> Loaded {
    let run = || match kind {
        Kind::Texture => load_texture(path).map(Loaded::Texture),
        Kind::Model => model::load(path).map(|m| Loaded::Model(Arc::new(m))),
        Kind::Audio => wave::peaks(path, WAVE_W as usize).map(|p| Loaded::Audio {
            rgba: wave::draw(&p.values, WAVE_W, WAVE_H, WAVE_COLOR),
            seconds: p.seconds,
            rate: p.rate,
            channels: p.channels,
        }),
    };
    std::panic::catch_unwind(run).unwrap_or_else(|_| Err("the file couldn't be decoded".to_owned())).unwrap_or_else(Loaded::Failed)
}

fn load_texture(path: &Path) -> Result<Arc<Image>, String> {
    const MAX_SIDE: u32 = 8192;
    let reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut img = reader.decode().map_err(|e| e.to_string())?;
    if img.width().max(img.height()) > MAX_SIDE { img = img.thumbnail(MAX_SIDE, MAX_SIDE); }
    let rgba = img.to_rgba8();
    Ok(Arc::new(Image { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }))
}

pub fn commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) { out.push(','); }
        out.push(c);
    }
    out
}

pub fn clock(seconds: f32) -> String {
    let tenths = (seconds * 10.0).floor() as u32;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

pub fn bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.0} KB", n as f64 / 1024.0),
        n if n < 1024 * 1024 * 1024 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        n => format!("{:.2} GB", n as f64 / 1_073_741_824.0),
    }
}

/// The sizes offered for a texture with `longest` as its longest side.
pub fn ladder(longest: u32) -> Vec<u32> {
    [2048u32, 1024, 512, 256, 128, 64].into_iter().filter(|s| *s < longest).take(5).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_like_people_write_them() {
        assert_eq!(commas(33081), "33,081");
        assert_eq!(commas(999), "999");
        assert_eq!(clock(83.45), "1:23.4");
        assert_eq!(bytes(2048), "2 KB");
    }

    #[test]
    fn the_ladder_only_offers_sizes_smaller_than_the_texture() {
        assert_eq!(ladder(1024), [512, 256, 128, 64]);
        assert_eq!(ladder(4096), [2048, 1024, 512, 256, 128]);
        assert!(ladder(64).is_empty());
    }
}
