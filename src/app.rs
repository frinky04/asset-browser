//! The application's state and everything that changes it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use rune::prefs::Prefs;
use rune::{Latest, Live, LiveText, Pixels, ScrollPos, Spring, TextEdit, Tx, Waker, WheelEvent};

use crate::audio::Audio;
use crate::editor::{AudioTab, Editor, ModelTab};
use crate::export::{self, Options};
use crate::library::{self, Filter, Kind, Library, Results, Sort};
use crate::model::{self, Image, Model};
use crate::query::Suggestion;
use crate::native_drag::{self, Ended};
use crate::stage::{Look, Scene, StageState, View, actual_size_zoom};
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
const MAIN_MIN_WIDTH: f32 = 620.0;
const INSPECTOR_PREF: &str = "inspector_width";
const SIDEBAR_PREF: &str = "sidebar_open";
const SIDEBAR_SPRING: Spring = Spring::new(0.3, 0.0);
/// How long a button shows that its action happened.
const CONFIRM_FOR: Duration = Duration::from_millis(1400);
/// How often a search on measured facts picks up what thumbnailing has found.
const MEASURE_EVERY: Duration = Duration::from_millis(800);

pub enum Preview {
    None,
    Loading,
    Texture(Arc<Image>),
    Model(Arc<Model>),
    Audio { seconds: f32, rate: u32, channels: u16 },
    Failed(String),
}

enum Loaded {
    Texture { full: Arc<Image>, shown: Arc<Image> },
    Model(Arc<Model>),
    Audio { rgba: Vec<u8>, seconds: f32, rate: u32, channels: u16 },
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    fn code(self) -> u8 {
        match self { Theme::System => 0, Theme::Light => 1, Theme::Dark => 2 }
    }

    fn from_code(code: u8) -> Theme {
        match code { 1 => Theme::Light, 2 => Theme::Dark, _ => Theme::System }
    }
}

/// How much the interface animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Motion {
    Off,
    Slow,
    Normal,
    Fast,
}

impl Motion {
    fn code(self) -> u8 {
        match self { Motion::Off => 0, Motion::Slow => 1, Motion::Normal => 2, Motion::Fast => 3 }
    }

    fn from_code(code: u8) -> Motion {
        match code { 0 => Motion::Off, 1 => Motion::Slow, 3 => Motion::Fast, _ => Motion::Normal }
    }

    /// How fast animations run compared to normal; 0 when there are none.
    pub fn speed(self) -> f32 {
        match self { Motion::Off => 0.0, Motion::Slow => 0.55, Motion::Normal => 1.0, Motion::Fast => 1.7 }
    }
}

const DEFAULT_TILE: f32 = 158.0;
const DEFAULT_VOLUME: f32 = 0.7;
const LIGHT_WAVE_COLOR: [u8; 3] = [72, 72, 80];

/// The preferences kept between launches, as they stood when last written.
#[derive(Clone, Debug, PartialEq)]
struct Saved {
    theme: u8,
    motion: u8,
    volume: f32,
    autoplay: bool,
    smooth: bool,
    nearest: bool,
    companions: bool,
    drag_max: Option<u32>,
    all_formats: bool,
    tile_size: f32,
}

impl Saved {
    fn load(prefs: &Prefs) -> Saved {
        Saved {
            theme: prefs.get("theme").unwrap_or(Theme::Dark.code()),
            motion: prefs.get("motion").unwrap_or(Motion::Normal.code()),
            volume: prefs.get("volume").unwrap_or(DEFAULT_VOLUME),
            autoplay: prefs.get("autoplay").unwrap_or(true),
            smooth: prefs.get("smooth").unwrap_or(false),
            nearest: prefs.get("nearest").unwrap_or(false),
            companions: prefs.get("companions").unwrap_or(true),
            drag_max: prefs.get("drag_max").unwrap_or(None),
            all_formats: prefs.get("all_formats").unwrap_or(false),
            tile_size: prefs.get("tile_size").unwrap_or(DEFAULT_TILE),
        }
    }

    fn write(&self, prefs: &mut Prefs) {
        let _ = prefs.set("theme", &self.theme);
        let _ = prefs.set("motion", &self.motion);
        let _ = prefs.set("volume", &self.volume);
        let _ = prefs.set("autoplay", &self.autoplay);
        let _ = prefs.set("smooth", &self.smooth);
        let _ = prefs.set("nearest", &self.nearest);
        let _ = prefs.set("companions", &self.companions);
        let _ = prefs.set("drag_max", &self.drag_max);
        let _ = prefs.set("all_formats", &self.all_formats);
        let _ = prefs.set("tile_size", &self.tile_size);
    }
}

/// Bytes of the thumbnail cache and of staged exports.
fn measure_storage() -> (u64, u64) {
    let thumbs = std::fs::metadata(export::app_dir().join("cache").join("thumbs.pack")).map_or(0, |m| m.len());
    (thumbs, export::staged_bytes())
}

/// Pressing an asset this size or smaller starts copying it at once. Bigger ones wait until
/// the pointer actually moves, so clicking through long recordings doesn't copy them all.
const EAGER_STAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Points of travel that show a press is becoming a drag, then that it is one.
const STAGE_TRAVEL: f32 = 1.5;
const DRAG_TRAVEL: f32 = 4.0;

/// The files and picture a drag hands to the OS, made ready off the UI thread.
struct Prepared {
    files: Vec<PathBuf>,
    image: Option<(u32, u32, Vec<u8>)>,
}

struct StageJob {
    id: u32,
    options: Options,
    result: Receiver<Result<Prepared, String>>,
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
    /// A preview at another size: the image it was made from, and the result.
    rescaling: Latest<(Arc<Image>, Arc<Image>)>,
    /// The size the selected texture is shown and dragged at, `None` being its own.
    pub texture_size: Option<u32>,
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
    pub settings_glide: Glide,
    /// Which button just confirmed an action, and when that confirmation ends.
    pub confirmed: Option<(&'static str, Instant)>,
    /// Whether the audio was playing when the view last ran.
    pub showing_playing: bool,
    /// Keeps checking playback while a sound is still opening.
    pub(crate) audio_watch_until: Option<Instant>,
    /// The Edit panel, and what's been asked of each asset.
    pub edit: Editor,
    inspector_w: f32,
    pub(crate) prefs: Prefs,
    pub columns: usize,
    pub tile_size: f32,
    /// The largest side textures are dragged out of the grid at.
    pub drag_max: Option<u32>,
    pub theme: Theme,
    pub motion: Motion,
    /// Whether the view last drew dark, which `Theme::System` decides at view time.
    pub resolved_dark: Cell<bool>,
    applied_dark: Option<bool>,
    pub settings_open: bool,
    /// The row picked with the arrow keys in the list under the search box.
    pub suggestion_at: Option<usize>,
    measured: usize,
    measured_at: Instant,
    pub volume: f32,
    /// Holds off saving until a slider is let go.
    pub volume_dragging: bool,
    /// Thumbnail cache and staged exports, in bytes, once measured.
    pub storage: Option<(u64, u64)>,
    storage_job: Latest<(u64, u64)>,
    saved: Saved,
    visible_rows: std::ops::Range<usize>,
    pub nearest: bool,
    pub companions: bool,
    pub status: LiveText,
    /// Why the last drag didn't work, or what it staged.
    pub message: Option<String>,
    drag_started: bool,
    /// The pointer has travelled far enough and the OS drag starts as soon as the files are ready.
    drag_wanted: bool,
    stage_job: Option<StageJob>,
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
    init_with(Prefs::open("asset-browser-v2"))
}

/// The app reading its saved settings from `prefs`.
pub fn init_with(prefs: Prefs) -> App {
    let inspector_w = prefs.get(INSPECTOR_PREF).unwrap_or(INSPECTOR_DEFAULT);
    let sidebar_open = prefs.get(SIDEBAR_PREF).unwrap_or(true);
    let saved = Saved::load(&prefs);
    let theme = Theme::from_code(saved.theme);
    let motion = Motion::from_code(saved.motion);
    let audio = Audio::new();
    audio.set_volume(saved.volume);
    App {
        lib: Arc::new(Library::default()),
        results: Results::default(),
        generation: 1,
        filter: Filter { all_formats: saved.all_formats, ..Filter::default() },
        search: TextEdit::new(""),
        thumbs: Thumbs::new(export::app_dir().join("cache")),
        audio,
        scanning: true,
        selected: None,
        preview: Preview::None,
        awaiting_preview: false,
        loading: Latest::default(),
        rescaling: Latest::default(),
        texture_size: saved.drag_max,
        stage: StageState { view: View { smooth: saved.smooth, ..View::default() }, ease: motion.speed(), ..StageState::default() },
        stage_size: [320.0, 320.0],
        scale: 1.0,
        drag_last: [0.0; 2],
        wave: Pixels::new(),
        playhead: Live::new([0.0, 0.0]),
        clock: LiveText::new("0:00.0"),
        autoplay: saved.autoplay,
        grid_width: 900.0,
        window_width: 1520.0,
        window_height: 900.0,
        sidebar_open,
        grid_glide: Glide::default(),
        packs_glide: Glide::default(),
        settings_glide: Glide::default(),
        confirmed: None,
        showing_playing: false,
        audio_watch_until: None,
        edit: Editor::default(),
        inspector_w,
        prefs,
        columns: 5,
        tile_size: saved.tile_size,
        drag_max: saved.drag_max,
        nearest: saved.nearest,
        companions: saved.companions,
        theme,
        motion,
        resolved_dark: Cell::new(theme != Theme::Light),
        applied_dark: None,
        settings_open: false,
        suggestion_at: None,
        measured: 0,
        measured_at: Instant::now(),
        volume: saved.volume,
        volume_dragging: false,
        storage: None,
        storage_job: Latest::default(),
        visible_rows: 0..0,
        saved,
        status: LiveText::new("Reading library"),
        message: None,
        drag_started: false,
        drag_wanted: false,
        stage_job: None,
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
    s.rescaling.set_waker(waker.clone());
    s.storage_job.set_waker(waker.clone());
    s.edit.set_waker(&waker);
    s.waker = Some(waker);
    let root = s.root.clone();
    s.start_loading(root);
    std::thread::spawn(export::prune);
}

fn headless() -> bool {
    rune::tooling::headless_run().is_some()
}

impl App {
    pub(crate) fn wake(&self) {
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
        if let Some(rest) = name.strip_prefix("light-") {
            self.theme = Theme::Light;
            self.run_scene(rest, tx);
            return;
        }
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
            "search-help" => self.search.focused = true,
            "search-values" => {
                self.search = TextEdit::new("brick is:");
                self.search.focused = true;
                self.search.set_caret(self.search.text.len());
                self.refilter(tx);
            }
            "small" => self.tile_size = 120.0,
            "settings" => self.open_settings(),
            "texture-sized" => {
                self.drag_max = Some(64);
                self.run_scene("texture", tx);
            }
            "settings-over-model" => {
                self.run_scene("model", tx);
                self.open_settings();
            }
            "start" => {}
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
            "edit-zoom" => {
                self.run_scene("edit-trim", tx);
                self.edit.view = Some((280_000, 520_000));
            }
            "edit-whole-model" => {
                self.run_scene("model", tx);
                self.edit.open = true;
                self.edit.model_tab = ModelTab::WholeModel;
                self.seed_scene_edit(crate::edit::Edit::Model(crate::edit::ModelEdit { origin: Some(crate::edit::mesh::Origin::Base), scale: Some(0.01), ..crate::edit::ModelEdit::default() }));
            }
            "edit-trim" | "edit-loop" | "edit-level" | "edit-compress" => {
                self.run_scene("sound", tx);
                self.edit.open = true;
                self.edit.audio_tab = match name { "edit-loop" => AudioTab::Loop, "edit-level" => AudioTab::Level, "edit-compress" => AudioTab::Compress, _ => AudioTab::Trim };
                let fades = if name == "edit-level" { (1_800, 3_000, -0.6) } else { (0, 0, 0.0) };
                let edit = crate::edit::AudioEdit { section: Some((300_000, 1_500_000)), looped: name == "edit-loop", crossfade_ms: 10, quality: 2, fade_in_ms: fades.0, fade_out_ms: fades.1, fade_out_shape: fades.2, ..crate::edit::AudioEdit::default() };
                self.seed_scene_edit(crate::edit::Edit::Audio(edit));
            }
            "edit-texture" => {
                self.run_scene("texture", tx);
                self.edit.open = true;
                self.seed_scene_edit(crate::edit::Edit::Image(crate::edit::ImageEdit { colors: Some(32), ..crate::edit::ImageEdit::default() }));
            }
            "edit-model" => {
                self.run_scene("model", tx);
                self.edit.open = true;
                self.edit.expanded = Some(0);
                let mut edit = crate::edit::ModelEdit::default();
                edit.materials.insert(0, crate::edit::MaterialEdit { roughness: Some(0.2), metallic: Some(0.9), ..crate::edit::MaterialEdit::default() });
                self.seed_scene_edit(crate::edit::Edit::Model(edit));
            }
            _ => {}
        }
    }

    pub fn poll(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
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
        // A snapshot shows finished work, so it waits for what the scene just asked for.
        if headless() { self.finish_preview_blocking(); }
        if let Some(loaded) = self.loading.take() {
            self.land_preview(loaded);
            changed = true;
        }
        if let Some((from, shown)) = self.rescaling.take() {
            self.land_rescaled(&from, shown);
            changed = true;
        }
        if self.poll_editor() { changed = true; }
        if self.step_view(tx) { changed = true; }
        // Reading a source starts its render, so one pass isn't enough for a snapshot.
        while headless() && self.edit.busy() {
            self.finish_preview_blocking();
            self.poll_editor();
            changed = true;
        }
        if self.advance_drag() { changed = true; }
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
        let dark = self.resolved_dark.get();
        if self.applied_dark != Some(dark) {
            self.apply_theme(dark);
            changed = true;
        }
        if let Some(storage) = self.storage_job.take() {
            self.storage = Some(storage);
            changed = true;
        }
        self.save_settings();
        if self.confirmed.is_some_and(|(_, until)| Instant::now() >= until) {
            self.confirmed = None;
            changed = true;
        }
        if matches!(self.preview, Preview::Audio { .. }) && self.track_playback(tx) { changed = true; }
        // A paused sound is still under way, so the button keeps offering Stop.
        let playing = !self.audio.is_finished();
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
        // A search on what's measured gains matches as thumbnailing measures more.
        let due = made == total || self.measured_at.elapsed() >= MEASURE_EVERY;
        if self.results.uses_facts && made != self.measured && due {
            self.measured = made;
            self.measured_at = Instant::now();
            self.refilter_keeping_scroll();
        }
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

    /// Moves the playhead along; true if a zoomed timeline had to page to keep up with it.
    fn track_playback(&mut self, tx: &mut Tx) -> bool {
        let Preview::Audio { seconds, .. } = self.preview else { return false };
        let position = self.shown_position(seconds);
        let paged = self.follow_playhead(position);
        if paged { tx.instant(); }
        self.sync_playhead();
        let opening = self.audio_watch_until.is_some_and(|t| Instant::now() < t);
        if self.audio.is_playing() || opening { tx.wake_after(Duration::from_millis(33)); }
        paged
    }

    pub(crate) fn wave_color(&self) -> [u8; 3] {
        if self.resolved_dark.get() { WAVE_COLOR } else { LIGHT_WAVE_COLOR }
    }

    /// Brings what isn't drawn by the view into line with the current theme.
    fn apply_theme(&mut self, dark: bool) {
        let first = self.applied_dark.is_none();
        self.applied_dark = Some(dark);
        self.stage.look = if dark { Look::DARK } else { Look::LIGHT };
        self.thumbs.set_wave_color(self.wave_color());
        self.redraw_seam();
        self.redraw_timeline();
        if first { return }
        self.generation += 1;
        self.request_range(self.visible_rows.clone());
        if matches!(self.preview, Preview::Audio { .. })
            && let Some(asset) = self.selected.and_then(|i| self.lib.assets.get(i as usize)).cloned()
        {
            self.request_preview(self.lib.path(&asset), asset.kind);
        }
    }

    pub fn set_theme(&mut self, theme: Theme, tx: &mut Tx) {
        self.theme = theme;
        tx.color_motion(rune::tok::motion::CALM);
    }

    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
        self.stage.ease = motion.speed();
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.audio.set_volume(self.volume);
    }

    fn save_settings(&mut self) {
        if self.volume_dragging || headless() { return }
        let now = Saved {
            theme: self.theme.code(),
            motion: self.motion.code(),
            volume: self.volume,
            autoplay: self.autoplay,
            smooth: self.stage.view.smooth,
            nearest: self.nearest,
            companions: self.companions,
            drag_max: self.drag_max,
            all_formats: self.filter.all_formats,
            tile_size: self.tile_size,
        };
        if now != self.saved {
            now.write(&mut self.prefs);
            self.saved = now;
        }
    }

    pub fn open_settings(&mut self) {
        self.settings_open = true;
        self.storage_job.request((), |()| measure_storage());
    }

    /// The texture the preview's model is showing by guesswork, if any.
    pub fn current_guess(&self) -> Option<&Path> {
        let Preview::Model(model) = &self.preview else { return None };
        model.guessed.first().map(PathBuf::as_path)
    }

    pub fn library_folder(&self) -> String {
        self.root.display().to_string()
    }

    pub fn rescan(&mut self) {
        let root = self.root.clone();
        self.start_loading(root);
    }

    /// What the list under the search box offers right now.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        if !self.search.focused || self.search.focus != self.search.text.len() { return Vec::new() }
        crate::query::suggestions(&self.search.text, &self.lib)
    }

    /// Puts a row of that list into the search box.
    pub fn accept_suggestion(&mut self, index: usize, tx: &mut Tx) {
        let Some(row) = self.suggestions().into_iter().nth(index) else { return };
        let text = crate::query::apply(&self.search.text, &row);
        let end = text.len();
        self.search.replace_text(text);
        self.search.set_caret(end);
        self.search.focused = true;
        self.refilter(tx);
    }

    /// Moves the picked row, or takes it off the list past either end.
    pub fn step_suggestion(&mut self, by: isize, count: usize) {
        let next = self.suggestion_at.map_or(if by > 0 { 0 } else { count as isize - 1 }, |at| at as isize + by);
        self.suggestion_at = (next >= 0 && (next as usize) < count).then_some(next as usize);
    }

    /// Empties the search and leaves the field ready for typing.
    pub fn clear_search(&mut self, tx: &mut Tx) {
        self.search = TextEdit::new("");
        self.search.focused = true;
        self.refilter(tx);
    }

    pub fn toggle_all_formats(&mut self) {
        self.filter.all_formats = !self.filter.all_formats;
        self.grid_glide.forget();
        self.refilter_keeping_scroll();
    }

    pub fn rebuild_thumbnails(&mut self) {
        self.thumbs.rebuild();
        self.thumbs.set_library(self.lib.clone());
        self.generation += 1;
        self.request_range(self.visible_rows.clone());
        self.storage_job.request((), |()| measure_storage());
    }

    pub fn clear_staged(&mut self) {
        self.storage_job.request((), |()| {
            export::clear_staged();
            measure_storage()
        });
    }

    /// The grid reports which rows are on screen.
    pub fn show_rows(&mut self, rows: std::ops::Range<usize>) {
        self.visible_rows = rows.clone();
        self.request_range(rows);
    }

    fn request_preview(&mut self, path: PathBuf, kind: Kind) {
        let wave_color = self.wave_color();
        self.awaiting_preview = true;
        let library = self.lib.root.clone();
        let (size, nearest) = (self.texture_size, self.nearest);
        self.loading.request((path, kind), move |(p, kind)| load_preview(&p, &library, kind, wave_color, size, nearest));
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
        let thumbs = &self.thumbs;
        self.results = library::search_with(&self.lib, &self.filter, &|key| thumbs.facts(key));
        self.generation += 1;
        self.request_range(0..FIRST_SCREEN_ROWS);
    }

    pub fn refilter(&mut self, tx: &mut Tx) {
        self.suggestion_at = None;
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
        self.texture_size = self.drag_max;
        let Some(asset) = self.lib.assets.get(id as usize).cloned() else { return };
        let path = self.lib.path(&asset);
        self.message = None;
        // The last preview stays up until this one lands, so stepping through assets doesn't
        // blank the panel between each.
        if matches!(self.preview, Preview::None) { self.preview = Preview::Loading; }
        self.awaiting_preview = true;
        self.audio.stop();
        self.editor_on_select();
        self.request_preview(path.clone(), asset.kind);
        if asset.kind == Kind::Audio && self.autoplay && !headless() {
            // An edited sound plays its edit, which isn't ready yet.
            if !self.defer_autoplay() { self.audio.play(path, 0.0); }
            self.watch_audio(tx);
        }
        tx.wake_after(Duration::from_millis(1));
    }

    fn finish_preview_blocking(&mut self) {
        let start = std::time::Instant::now();
        while (self.edit.busy() || self.awaiting_preview && (self.loading.is_pending() || self.rescaling.is_pending())) && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Whether the selected asset's preview has arrived.
    #[cfg(test)]
    pub fn preview_landed(&self) -> bool {
        !self.awaiting_preview
    }

    /// Pixels of the texture the preview is showing now.
    pub fn shown_size(&self) -> Option<(u32, u32)> {
        match &self.stage.scene {
            Scene::Texture(image) => Some((image.width, image.height)),
            _ => None,
        }
    }

    /// Shows and drags the selected texture at `size`.
    pub fn set_texture_size(&mut self, size: Option<u32>) {
        self.texture_size = size;
        self.rescale_preview();
    }

    /// The size textures start at, and the one grid drags use.
    pub fn set_drag_max(&mut self, size: Option<u32>) {
        self.drag_max = size;
        self.set_texture_size(size);
    }

    pub fn set_nearest(&mut self, nearest: bool) {
        self.nearest = nearest;
        self.rescale_preview();
    }

    /// Brings the selected texture to the current size, re-rendering it if it carries an edit.
    fn rescale_preview(&mut self) {
        if self.has_image_edit() {
            self.edit_changed();
            if !self.edit.comparing { return }
        }
        self.show_plain_size();
    }

    /// Shows the selected texture unedited, at the current size.
    pub(crate) fn show_plain_size(&mut self) {
        let Preview::Texture(full) = &self.preview else { return };
        let (full, size, nearest) = (full.clone(), self.texture_size, self.nearest);
        self.awaiting_preview = true;
        self.rescaling.request(full, move |full| {
            let shown = at_size(&full, size, nearest);
            (full, shown)
        });
    }

    /// Swaps the picture without moving the view, unless it's at 1:1, which is per pixel.
    fn land_rescaled(&mut self, from: &Arc<Image>, shown: Arc<Image>) {
        let Preview::Texture(full) = &self.preview else { return };
        if !Arc::ptr_eq(full, from) { return }
        self.awaiting_preview = false;
        // An older plain resize can finish after an edit has taken the stage.
        if self.has_image_edit() && !self.edit.comparing { return }
        self.show_texture(shown);
    }

    pub(crate) fn show_texture(&mut self, shown: Arc<Image>) {
        let at_actual_size = self.shown_size().is_some_and(|size| !self.stage.view.tile && (self.stage.view.zoom - actual_size_zoom(self.stage_size, size, self.scale)).abs() < 1e-3);
        let size = (shown.width, shown.height);
        self.stage.scene = Scene::Texture(shown);
        self.stage.revision += 1;
        if at_actual_size { self.stage.view.zoom = actual_size_zoom(self.stage_size, size, self.scale); }
    }

    fn land_preview(&mut self, loaded: Loaded) {
        self.awaiting_preview = false;
        let keep = self.stage.view;
        self.stage.view = View { tile: keep.tile, seams: keep.seams, smooth: keep.smooth, zoom: if keep.tile { 0.34 } else { 1.0 }, ..View::default() };
        self.playhead.set([0.0, 0.0]);
        self.preview = match loaded {
            Loaded::Texture { full, shown } => {
                self.stage.scene = Scene::Texture(shown);
                self.stage.revision += 1;
                Preview::Texture(full)
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
        self.editor_after_preview();
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
        if !self.audio_selected() { return }
        if self.audio.is_finished() {
            self.play_from_cue(tx);
        } else {
            self.audio.stop();
            self.reveal_time(self.cue_point());
            self.sync_playhead();
        }
    }

    /// Holds playback where it is, or carries on from there.
    pub fn pause_playback(&mut self, tx: &mut Tx) {
        if !self.audio_selected() { return }
        if self.audio.is_finished() {
            self.play_from_cue(tx);
        } else {
            self.audio.toggle();
            self.watch_audio(tx);
        }
    }

    /// Back to the start of the section or sound, forgetting any clicked point.
    pub fn rewind_playback(&mut self, tx: &mut Tx) {
        if !self.audio_selected() { return }
        self.edit.cue = None;
        let at = self.cue_point();
        if !self.audio.is_finished() { self.seek_live(at); }
        self.reveal_time(at);
        self.sync_playhead();
        tx.wake_after(Duration::from_millis(33));
    }

    pub(crate) fn audio_selected(&self) -> bool {
        self.selected_asset().is_some_and(|a| a.kind == Kind::Audio)
    }

    fn play_from_cue(&mut self, tx: &mut Tx) {
        let Some(asset) = self.selected_asset() else { return };
        let at = self.cue_point();
        if !self.play_audition_from(at) {
            self.edit.timeline = None;
            self.audio.play(self.lib.path(&asset), at);
        }
        self.reveal_time(at);
        self.watch_audio(tx);
    }

    /// Moves a sound that's already going to `at` seconds.
    fn seek_live(&mut self, at: f32) {
        if !self.seek_audition(at) { self.audio.seek(at); }
    }

    /// Puts the playhead and clock where playback is, or at the cue when nothing's playing.
    pub(crate) fn sync_playhead(&mut self) {
        let Preview::Audio { seconds, .. } = self.preview else { return };
        let position = self.shown_position(seconds);
        self.playhead.set([self.timeline_x(position, seconds), 0.0]);
        self.clock.set(format!("{} / {}", clock(position), clock(seconds)));
    }

    fn shown_position(&self, seconds: f32) -> f32 {
        if self.audio.is_finished() { self.cue_point().min(seconds) } else { self.playback_time().min(seconds) }
    }

    fn watch_audio(&mut self, tx: &mut Tx) {
        self.audio_watch_until = Some(Instant::now() + Duration::from_millis(600));
        tx.wake_after(Duration::from_millis(33));
    }

    pub fn seek_fraction(&mut self, fraction: f32, tx: &mut Tx) {
        let Preview::Audio { seconds, .. } = self.preview else { return };
        let at = fraction.clamp(0.0, 1.0) * seconds;
        // A click marks where playback starts. Only sound already going moves there at once.
        self.edit.cue = Some(at);
        if !self.audio.is_finished() { self.seek_live(at); }
        self.sync_playhead();
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

    /// How asset `id` leaves when dragged at `max_size`, with whatever edit it carries.
    pub fn options(&self, id: u32, max_size: Option<u32>) -> Options {
        let edit = self.lib.assets.get(id as usize).and_then(|a| self.edit.recipe(a.key));
        Options { max_size, nearest: self.nearest, companions: self.companions, edit }
    }

    /// Prepares what a drag of `id` will hand over, off the UI thread. Pressing and dragging
    /// both ask, so this does nothing when that exact job is already under way.
    fn start_stage_job(&mut self, id: u32, options: Options) {
        if self.stage_job.as_ref().is_some_and(|j| j.id == id && j.options == options) { return }
        let Some(asset) = self.lib.assets.get(id as usize).cloned() else { return };
        let path = self.lib.path(&asset);
        let thumbs = self.thumbs.clone();
        let waker = self.waker.clone();
        let (send, result) = channel();
        self.stage_job = Some(StageJob { id, options: options.clone(), result });
        let work = move || {
            let prepared = export::stage(&path, &asset, &options).map(|files| Prepared { files, image: thumbs.image(asset.key).map(drag_picture) });
            let _ = send.send(prepared);
            if let Some(w) = &waker { w(); }
        };
        if headless() { work(); } else { std::thread::spawn(work); }
    }

    /// The drag gesture on a tile or a size rung.
    pub fn drag_gesture(&mut self, id: u32, max_size: Option<u32>, drag: &rune::Drag) {
        use rune::Phase;
        let options = self.options(id, max_size);
        match drag.phase {
            Phase::Down => {
                self.drag_started = false;
                self.drag_wanted = false;
                self.stage_job = None;
                // Cheap to copy, so it gets a head start even if this is only a click. An edit
                // may need rendering first, and that waits until it's really a drag.
                if options.edit.is_none() && self.lib.assets.get(id as usize).is_some_and(|a| a.size <= EAGER_STAGE_BYTES) { self.start_stage_job(id, options); }
            }
            Phase::Move => {
                let travel = drag.dx.hypot(drag.dy);
                if travel >= STAGE_TRAVEL { self.start_stage_job(id, options); }
                if !self.drag_started && travel > DRAG_TRAVEL {
                    self.drag_started = true;
                    self.drag_wanted = true;
                    self.advance_drag();
                }
            }
            Phase::Up => {
                self.drag_started = false;
                self.drag_wanted = false;
            }
        }
    }

    /// Starts the OS drag once it's wanted and its files are ready; true if anything changed.
    fn advance_drag(&mut self) -> bool {
        if !self.drag_wanted { return false }
        let Some(job) = &self.stage_job else { return false };
        let prepared = match job.result.try_recv() {
            Ok(prepared) => prepared,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the copy stopped early".to_owned()),
        };
        let id = job.id;
        self.drag_wanted = false;
        match prepared {
            Err(reason) => {
                let name = self.lib.assets.get(id as usize).map_or("the file", |a| a.name());
                self.message = Some(format!("Couldn't prepare {name}: {reason}"));
            }
            Ok(Prepared { files, image }) => {
                self.last_staged = files.clone();
                self.drag_label = files.first().and_then(|f| f.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                if headless() {
                    self.message = Some(format!("Staged {}", self.drag_label));
                } else {
                    self.dragging = Some(id);
                    native_drag::begin(native_drag::Request { files, image, done: self.drag_results.0.clone() });
                }
            }
        }
        true
    }

    /// Whether a drag is still waiting for its files.
    #[cfg(test)]
    pub fn drag_waiting(&self) -> bool {
        self.drag_wanted
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

/// `full` with its longest side at `max`, or itself if that wouldn't make it smaller.
fn at_size(full: &Arc<Image>, max: Option<u32>, nearest: bool) -> Arc<Image> {
    let Some(max) = max.filter(|m| *m < full.width.max(full.height)) else { return full.clone() };
    let Some(pixels) = image::RgbaImage::from_raw(full.width, full.height, full.rgba.clone()) else { return full.clone() };
    let out = export::scale(&image::DynamicImage::ImageRgba8(pixels), max, nearest).into_rgba8();
    Arc::new(Image { width: out.width(), height: out.height(), rgba: out.into_raw() })
}

fn load_preview(path: &Path, library: &Path, kind: Kind, wave_color: [u8; 3], size: Option<u32>, nearest: bool) -> Loaded {
    let run = || match kind {
        Kind::Texture => load_texture(path).map(|full| {
            let shown = at_size(&full, size, nearest);
            Loaded::Texture { full, shown }
        }),
        Kind::Model => model::load(path, library).map(|m| Loaded::Model(Arc::new(m))),
        Kind::Audio => wave::peaks(path, WAVE_W as usize).map(|p| Loaded::Audio {
            rgba: wave::draw(&p.values, WAVE_W, WAVE_H, wave_color),
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
