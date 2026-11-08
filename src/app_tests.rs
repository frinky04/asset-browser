//! The whole app driven through its real view: pointer and key events in, state and files out.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rune::{Env, Key, KeyEvent, Runtime};

use crate::app::{self, App, Preview};
use crate::library::{self, Kind};
use crate::ui;

/// Points the app's cache and saved window layout away from the real ones, once per run.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ab-isolated-{}", std::process::id()));
        // SAFETY: set once, before any test here reads them.
        unsafe {
            std::env::set_var("ASSET_BROWSER_DATA", dir.join("data"));
            std::env::set_var("RUNE_PREFS_DIR", dir.join("prefs"));
        }
    });
}

#[test]
fn the_filter_row_stays_inside_its_panel_at_every_width() {
    isolate();
    let mut files = Vec::new();
    for ext in ["png", "jpg", "jpeg", "bmp", "tga", "webp", "glb", "gltf", "obj", "dae", "fbx", "wav", "ogg", "mp3", "flac"] {
        for i in 0..3 { files.push(library::Entry { rel: format!("Pack/{ext}_{i}.{ext}"), size: 1, mtime: 1 }); }
    }
    let mut state = app::init_with(rune::prefs::Prefs::memory());
    state.lib = Arc::new(library::Library::build(PathBuf::from("Z:/none"), files));
    state.results = library::search(&state.lib, &state.filter);
    state.scanning = false;
    let mut rt = Runtime::new(state, ui::view, (1520.0, 900.0), Env::new(true, 1.0));
    settle(&mut rt);
    let rect_of = |rt: &Runtime<App>, suffix: &str| rt.elements().into_iter().find(|e| e.path.ends_with(suffix)).unwrap_or_else(|| panic!("no {suffix}")).rect;
    for open in [true, false] {
        rt.state.sidebar_open = open;
        for inspector in [300.0, 372.0, 520.0, 760.0] {
            for width in (1200..=2100).step_by(19) {
                rt.state.resize_inspector(inspector);
                rt.set_viewport((width as f32, 900.0), 1.0);
                rt.invalidate();
                for _ in 0..3 { rt.frame(1.0); }
                let (main, group, tabs) = (rect_of(&rt, "panels/main"), rect_of(&rt, "filters/format-group"), rect_of(&rt, "filters/kinds"));
                let at = format!("window {width}, inspector {inspector}, sidebar {open}, grid {}", rt.state.grid_width);
                assert!(group[0] + group[2] <= main[0] + main[2] - 15.0, "formats past the panel at {at}: {group:?} in {main:?}");
                assert!(tabs[0] + tabs[2] <= group[0], "tabs overlap the formats at {at}");
            }
        }
    }
}

#[test]
fn settings_open_apply_and_survive_a_restart() {
    isolate();
    let mut state = app::init_with(rune::prefs::Prefs::memory());
    state.lib = Arc::new(library::Library::build(PathBuf::from("Z:/none"), vec![library::Entry { rel: "Pack/a.png".into(), size: 1, mtime: 1 }]));
    state.results = library::search(&state.lib, &state.filter);
    state.scanning = false;
    let mut rt = Runtime::new(state, ui::view, (1520.0, 900.0), Env::new(true, 1.0));
    settle(&mut rt);

    click(&mut rt, "settings-button");
    assert!(rt.state.settings_open);
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "theme/Light");
    assert_eq!(rt.state.theme, app::Theme::Light);
    settle(&mut rt);
    assert!(!rt.state.resolved_dark.get(), "the view drew with the light palette");
    let (x, y) = rt.find("volume").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    rt.pointer_up(x, y);
    settle(&mut rt);
    assert!((rt.state.volume - 0.5).abs() < 0.02, "the middle of the slider is half: {}", rt.state.volume);
    click(&mut rt, "Play sounds on select");
    assert!(!rt.state.autoplay);
    rt.update(|s, tx| { s.poll(tx); });
    assert_eq!(rt.state.stage.look, crate::stage::Look::LIGHT, "the preview follows the theme");

    // Escape closes it, and keys don't reach the grid underneath meanwhile.
    rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
    settle(&mut rt);
    assert!(!rt.state.settings_open);

    // Smart search is off until asked for, and its speed is remembered.
    assert!(!rt.state.smart_on);
    assert_eq!(rt.state.index_effort, crate::smart::Effort::Cpu, "the graphics card has to be chosen");
    rt.state.set_index_effort(crate::smart::Effort::Paused);
    rt.update(|s, tx| { s.poll(tx); });

    // A new launch reads what was saved.
    let again = app::init_with(std::mem::replace(&mut rt.state.prefs, rune::prefs::Prefs::memory()));
    assert_eq!(again.index_effort, crate::smart::Effort::Paused);
    assert_eq!(again.theme, app::Theme::Light);
    assert!(!again.autoplay);
    assert!((again.volume - rt.state.volume).abs() < 1e-3);
}

/// One app per motion setting, each with a list that overflows, measuring two animations a
/// fixed number of frames in: the sidebar's closing width and a wheel notch's travel.
fn motion_samples(motion: app::Motion) -> (f32, f32) {
    isolate();
    let files = (0..400).map(|i| library::Entry { rel: format!("Pack/a{i}.png"), size: 1, mtime: 1 }).collect();
    let mut state = app::init_with(rune::prefs::Prefs::memory());
    state.lib = Arc::new(library::Library::build(PathBuf::from("Z:/none"), files));
    state.results = library::search(&state.lib, &state.filter);
    state.scanning = false;
    state.set_motion(motion);
    let mut rt = Runtime::new(state, ui::view, (1520.0, 900.0), Env::new(true, 1.0));
    for _ in 0..30 { rt.frame(1.0 / 60.0); }

    let (x, y) = rt.find("grid").unwrap();
    rt.pointer_move(x, y);
    let start = rt.presented("rows").unwrap().0[1];
    rt.wheel_event(rune::WheelEvent::new(0.0, -40.0).at(x, y).precise(false));
    for _ in 0..4 { rt.frame(1.0 / 60.0); }
    let travelled = start - rt.presented("rows").unwrap().0[1];
    for _ in 0..80 { rt.frame(1.0 / 60.0); }

    let (x, y) = rt.find("sidebar-toggle").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..5 { rt.frame(1.0 / 60.0); }
    (rt.presented("sidebar").unwrap().0[2], travelled)
}

#[test]
fn the_animation_setting_changes_how_fast_things_move() {
    let [off, slow, normal, fast] = [app::Motion::Off, app::Motion::Slow, app::Motion::Normal, app::Motion::Fast].map(motion_samples);
    assert!(off.0 < 1.0, "off closes at once: {off:?}");
    assert!(slow.0 > normal.0 && normal.0 > fast.0, "the sidebar follows the speed: {slow:?} {normal:?} {fast:?}");
    assert!(slow.1 < normal.1 && normal.1 < fast.1, "so does a wheel glide: {slow:?} {normal:?} {fast:?}");
    assert!((off.1 - 100.0).abs() < 1.0, "off scrolls at once: {off:?}");
}

fn fixture() -> (PathBuf, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("ab-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("lib");
    std::fs::create_dir_all(root.join("Bricks")).unwrap();
    std::fs::create_dir_all(root.join("Props")).unwrap();
    for i in 0..12 {
        image::RgbaImage::from_pixel(800, 400, image::Rgba([i * 20, 40, 60, 255])).save(root.join(format!("Bricks/brick_{i}.png"))).unwrap();
    }
    std::fs::write(root.join("Props/crate.obj"), "v -1 -1 0\nv 1 -1 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    isolate();
    let bytes = std::fs::read(root.join("Bricks/brick_0.png")).unwrap();
    (root, bytes)
}

fn app_on(root: &Path) -> Runtime<App> {
    let mut state = app::init_with(rune::prefs::Prefs::memory());
    state.lib = Arc::new(library::Library::build(root.to_path_buf(), library::walk(root)));
    state.thumbs.set_library(state.lib.clone());
    state.results = library::search(&state.lib, &state.filter);
    state.scanning = false;
    let mut rt = Runtime::new(state, ui::view, (1520.0, 900.0), Env::new(true, 1.0));
    settle(&mut rt);
    rt
}

fn settle(rt: &mut Runtime<App>) {
    for _ in 0..8 { rt.frame(1.0 / 60.0); }
}

fn tile_key(rt: &Runtime<App>, name: &str) -> String {
    let id = rt.state.lib.assets.iter().position(|a| a.name() == name).unwrap();
    format!("a{id}")
}

fn click(rt: &mut Runtime<App>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(rt);
}

/// Lets the handlers run, then waits for a drag's files, which are prepared off the UI thread.
fn pump_drag(rt: &mut Runtime<App>) {
    settle(rt);
    let start = Instant::now();
    while rt.state.drag_waiting() && start.elapsed() < Duration::from_secs(10) {
        rt.update(|s, tx| { s.poll(tx); });
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(rt);
}

fn wait_for_preview(rt: &mut Runtime<App>) {
    let start = Instant::now();
    while !rt.state.preview_landed() && start.elapsed() < Duration::from_secs(10) {
        rt.update(|s, tx| { s.poll(tx); });
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(rt);
}

#[test]
fn browsing_previewing_and_dragging_out_end_to_end() {
    let (root, original) = fixture();
    let mut rt = app_on(&root);

    // The grid shows everything, in natural order, as tiles.
    assert_eq!(rt.state.results.ids.len(), 13);
    assert_eq!(rt.state.results.by_kind, [12, 1, 0]);
    assert!(rt.find(&tile_key(&rt, "brick_0.png")).is_some());

    // Filtering by kind leaves only that kind.
    click(&mut rt, "Models 1");
    assert_eq!(rt.state.filter.kind, Some(Kind::Model));
    assert_eq!(rt.state.results.ids.len(), 1);
    click(&mut rt, "All 13");

    // Every chooser's indicator sits evenly inside its frame, on all four sides.
    let rect_of = |rt: &Runtime<App>, suffix: &str| rt.elements().into_iter().find(|e| e.path.ends_with(suffix)).unwrap_or_else(|| panic!("no {suffix}")).rect;
    for group in ["kinds", "sort", "dragmax", "tilesize"] {
        let (outer, inner) = (rect_of(&rt, group), rect_of(&rt, &format!("{group}/indicator")));
        let (top, bottom) = (inner[1] - outer[1], outer[1] + outer[3] - inner[1] - inner[3]);
        assert_eq!(top, bottom, "{group}: {top} above, {bottom} below");
        let last_right = outer[0] + outer[2] - 3.0;
        assert!(inner[0] >= outer[0] + 3.0 && inner[0] + inner[2] <= last_right + 0.01, "{group}: {inner:?} in {outer:?}");
    }

    // Neighbours in the title bar share a height and sit on one line.
    let (sort, search) = (rect_of(&rt, "titlebar/sort"), rect_of(&rt, "titlebar/searchbar"));
    assert_eq!((sort[1], sort[3]), (search[1], search[3]), "sort {sort:?}, search {search:?}");

    // Choosing a format changes the counts, but none of the tabs may move.
    let tabs_before = rect_of(&rt, "filters/kinds");
    click(&mut rt, "png 12");
    assert_eq!(rt.state.filter.ext, Some("png"));
    assert_eq!(rect_of(&rt, "filters/kinds"), tabs_before, "the tabs keep their place");
    click(&mut rt, "png 12");

    // A pack in the sidebar narrows to it.
    click(&mut rt, "Bricks");
    assert_eq!(rt.state.results.ids.len(), 12);
    click(&mut rt, "All packs");

    // Clicking selects; the size buttons offer smaller sizes and never larger.
    let brick = tile_key(&rt, "brick_0.png");
    click(&mut rt, &brick);
    wait_for_preview(&mut rt);
    assert!(matches!(rt.state.preview, Preview::Texture(_)));
    assert!(rt.find("texture-size/512").is_some() && rt.find("texture-size/1024").is_none());
    assert_eq!(rt.state.shown_size(), Some((800, 400)));

    // A size button changes what's shown, and the drag follows it.
    click(&mut rt, "texture-size/256");
    wait_for_preview(&mut rt);
    assert_eq!(rt.state.shown_size(), Some((256, 128)));
    assert_eq!(rt.state.drag_max, None, "only this texture changed");
    let source = root.join("Bricks/brick_0.png");
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let (x, y) = rt.find("drag-card").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x + 30.0, y + 10.0);
    pump_drag(&mut rt);
    rt.pointer_up(x + 30.0, y + 10.0);
    settle(&mut rt);
    let staged = rt.state.last_staged.clone();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].file_name().unwrap(), "brick_0_256.png");
    assert_eq!(image::image_dimensions(&staged[0]).unwrap(), (256, 128));
    assert_eq!(std::fs::read(&source).unwrap(), original, "source bytes unchanged");
    assert_eq!(std::fs::metadata(&source).unwrap().modified().unwrap(), modified);
    assert!(!staged[0].starts_with(&root), "the staged copy lives outside the library");

    // Full goes back, and the filter shows in the preview too.
    click(&mut rt, "texture-size/Full");
    wait_for_preview(&mut rt);
    assert_eq!(rt.state.shown_size(), Some((800, 400)));

    // The tile honours the toolbar's drag size, and so does the next texture opened.
    click(&mut rt, "dragmax/512");
    wait_for_preview(&mut rt);
    assert_eq!(rt.state.drag_max, Some(512));
    assert_eq!(rt.state.shown_size(), Some((512, 256)));
    let (x, y) = rt.find(&tile_key(&rt, "brick_1.png")).unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x - 40.0, y);
    pump_drag(&mut rt);
    rt.pointer_up(x - 40.0, y);
    settle(&mut rt);
    assert_eq!(rt.state.last_staged[0].file_name().unwrap(), "brick_1_512.png");
    // Released over the tile it came from, the drag also selected it.
    wait_for_preview(&mut rt);

    // Tile mode and panning a texture move the stage's view, not the view tree.
    click(&mut rt, "Tile");
    assert!(rt.state.stage.view.tile);
    let (x, y) = rt.find("stage").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    // The debug build re-runs the view now and then to audit `live_only`; don't count those.
    rt.check_live_only(false);
    let commits = rt.stats.commits;
    for step in 1..=10 { rt.pointer_move(x + step as f32 * 4.0, y); rt.frame(1.0 / 60.0); }
    rt.pointer_up(x + 40.0, y);
    assert!((rt.state.stage.view.offset[0] - 40.0).abs() < 0.5, "{:?}", rt.state.stage.view.offset);
    assert_eq!(rt.stats.commits, commits, "panning doesn't rebuild the view");
    click(&mut rt, "Tile");

    // The handle beside the inspector resizes it, within bounds, and the grid follows.
    let before = (rt.state.inspector_width(), rt.state.grid_width);
    let (x, y) = rt.find("inspector-handle").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    rt.pointer_move(x - 120.0, y);
    settle(&mut rt);
    rt.pointer_up(x - 120.0, y);
    settle(&mut rt);
    assert_eq!(rt.state.inspector_width(), before.0 + 120.0);
    assert!((rt.state.grid_width - (before.1 - 120.0)).abs() < 1.0, "{} from {}", rt.state.grid_width, before.1);
    let (x, y) = rt.find("inspector-handle").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    rt.pointer_move(x + 1000.0, y);
    settle(&mut rt);
    rt.pointer_up(x + 1000.0, y);
    settle(&mut rt);
    assert_eq!(rt.state.inspector_width(), 300.0, "stops at the minimum");

    // A model opens in the stage, and dragging orbits while Shift-free.
    click(&mut rt, "All 13");
    let model = tile_key(&rt, "crate.obj");
    let row = rt.state.results.ids.iter().position(|i| format!("a{i}") == model).unwrap() / rt.state.columns;
    rt.update(move |_, tx| tx.scroll_into_view(format!("grid/rows/r{row}")));
    settle(&mut rt);
    click(&mut rt, &model);
    wait_for_preview(&mut rt);
    assert!(matches!(rt.state.preview, Preview::Model(_)));
    let (x, y) = rt.find("stage").unwrap();
    let before = rt.state.stage.view.yaw;
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    rt.pointer_move(x + 50.0, y + 5.0);
    rt.frame(1.0 / 60.0);
    rt.pointer_up(x + 50.0, y + 5.0);
    assert!((rt.state.stage.view.yaw - before - 50.0 * 0.009).abs() < 1e-3);

    // The panel collapses and comes back, and the grid takes its room.
    let open_width = rt.state.grid_width;
    click(&mut rt, "sidebar-toggle");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(!rt.state.sidebar_open);
    assert!((rt.state.grid_width - (open_width + 252.0)).abs() < 1.0, "{} from {open_width}", rt.state.grid_width);
    assert!(rt.presented("sidebar").unwrap().0[2] < 1.0, "fully closed");
    click(&mut rt, "sidebar-toggle");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(rt.state.sidebar_open);
    assert!((rt.state.grid_width - open_width).abs() < 1.0);

    // Copying says so, and the button keeps its size either way.
    let width_of = |rt: &Runtime<App>| rt.presented("Copy path").unwrap().0[2];
    let before = width_of(&rt);
    click(&mut rt, "Copy path");
    assert!(rt.state.is_confirmed("copy"));
    assert_eq!(width_of(&rt), before, "the label swap doesn't resize it");

    // A notched wheel eases toward its target instead of jumping there.
    click(&mut rt, "L");
    for _ in 0..20 { rt.frame(1.0 / 60.0); }
    let grid_top = |rt: &Runtime<App>| rt.presented("rows").unwrap().0[1];
    let start = grid_top(&rt);
    let (x, y) = rt.find("grid").unwrap();
    rt.pointer_move(x, y);
    let notch = rune::WheelEvent::new(0.0, -40.0).at(x, y).precise(false);
    rt.wheel_event(notch);
    rt.frame(1.0 / 120.0);
    let first = start - grid_top(&rt);
    for _ in 0..80 { rt.frame(1.0 / 60.0); }
    let settled = start - grid_top(&rt);
    assert!((settled - 100.0).abs() < 1.0, "one notch travels 100 points, moved {settled}");
    assert!(first < settled * 0.8, "moved {first} of {settled} on the first frame");

    // Arrow keys walk the grid.
    let left = KeyEvent { key: Key::Left, mods: Default::default() };
    rt.update(move |s, tx| { ui::on_key(s, &left, tx); });
    settle(&mut rt);
    let at = rt.state.selected.unwrap();
    assert_eq!(rt.state.lib.assets[at as usize].name(), "brick_11.png");

    // No search hit, no tiles: and the message says what to do.
    rt.update(|s, tx| { s.search = rune::TextEdit::new("nothing-called-this"); s.refilter(tx); });
    settle(&mut rt);
    assert_eq!(rt.state.results.ids.len(), 0);
    assert!(rt.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.starts_with("Nothing matches"))));

    // The X clears the search, brings everything back and goes away.
    click(&mut rt, "search-clear");
    assert!(rt.state.search.text.is_empty());
    assert_eq!(rt.state.results.ids.len(), 13);
    assert!(rt.find("search-clear").is_none());

    drop(rt);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

/// A library with a two-second tone, a noisy JPEG and a textured OBJ.
fn edit_fixture() -> PathBuf {
    isolate();
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ab-edit-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Pack")).unwrap();
    let samples: Vec<i16> = (0..88_200).map(|i| ((i as f32 * 0.04).sin() * 12_000.0) as i16).collect();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples.len() as u32 * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&44_100u32.to_le_bytes());
    wav.extend_from_slice(&88_200u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for s in &samples { wav.extend_from_slice(&s.to_le_bytes()); }
    std::fs::write(root.join("Pack/tone.wav"), wav).unwrap();
    let noise = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, ((x * y) % 256) as u8]));
    noise.save(root.join("Pack/noise.jpg")).unwrap();
    image::RgbaImage::from_pixel(128, 128, image::Rgba([120, 80, 40, 255])).save(root.join("Pack/skin.png")).unwrap();
    std::fs::write(root.join("Pack/tri.mtl"), "newmtl hull\nmap_Kd skin.png\n").unwrap();
    std::fs::write(root.join("Pack/tri.obj"), "mtllib tri.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nvn 0 0 1\nusemtl hull\nf 1/1/1 2/2/1 3/3/1\n").unwrap();
    root
}

fn wait_for_edit(rt: &mut Runtime<App>) {
    // A pointer event's handler can start the work on the next frame.
    settle(rt);
    let start = Instant::now();
    // One job's answer often starts the next, so it's only over when polling finds nothing more.
    let mut quiet = 0;
    while quiet < 3 && start.elapsed() < Duration::from_secs(30) {
        let mut landed = false;
        rt.update(|s, tx| landed = s.poll_editor_for_test(tx));
        quiet = if landed || rt.state.edit.busy() { 0 } else { quiet + 1 };
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(rt);
}

/// Selects `name` and opens the Edit panel on it, ready to use.
fn open_editor(rt: &mut Runtime<App>, name: &str) {
    let tile = tile_key(rt, name);
    click(rt, &tile);
    wait_for_preview(rt);
    if !rt.state.edit.open { click(rt, "edit-toggle"); }
    wait_for_edit(rt);
    assert!(rt.find("edit-panel").is_some());
}

/// Drags the white bar out and returns what got staged.
fn drag_out(rt: &mut Runtime<App>, key: &str) -> Vec<PathBuf> {
    let (x, y) = rt.find(key).unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x + 30.0, y + 10.0);
    pump_drag(rt);
    rt.pointer_up(x + 30.0, y + 10.0);
    settle(rt);
    rt.state.last_staged.clone()
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = walkdir::WalkDir::new(root).into_iter().flatten().filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_owned(), std::fs::read(e.path()).unwrap())).collect();
    files.sort();
    files
}

#[test]
fn trimming_audio_changes_the_preview_and_the_drag_but_never_the_library() {
    let root = edit_fixture();
    let before = snapshot_tree(&root);
    let mut rt = app_on(&root);

    // Nothing about editing shows until it's asked for.
    let tile = tile_key(&rt, "tone.wav");
    click(&mut rt, &tile);
    wait_for_preview(&mut rt);
    assert!(rt.find("edit-panel").is_none() && rt.find("mark-end").is_none());

    open_editor(&mut rt, "tone.wav");
    assert_eq!(rt.state.edit.output_bytes(), None, "an untouched asset renders nothing");

    // Dragging the end marker back keeps the first part.
    let (x, y) = rt.find("mark-end").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    rt.pointer_move(x - 150.0, y);
    settle(&mut rt);
    rt.pointer_up(x - 150.0, y);
    wait_for_edit(&mut rt);
    let (start, end, frames) = rt.state.audio_section().unwrap();
    assert!(start == 0 && end < frames * 3 / 4, "{start}..{end} of {frames}");
    assert!(rt.state.edit.output_bytes().is_some(), "the edit rendered");

    // The dragged file is that section, as Ogg.
    let staged = drag_out(&mut rt, "drag-card");
    assert_eq!(staged[0].file_name().unwrap(), "tone.ogg");
    // Counted by libvorbis, which is what most game engines read Ogg with.
    let mut decoder = vorbis_rs::VorbisDecoder::new(std::fs::File::open(&staged[0]).unwrap()).unwrap();
    let mut heard = 0u64;
    while let Some(block) = decoder.decode_audio_block().unwrap() { heard += block.samples()[0].len() as u64; }
    assert_eq!(heard, end);

    // Quality changes the file's size, and Reset puts it all back.
    click(&mut rt, "audio-tab/Compress");
    rt.update(|s, _| s.set_quality(0, true));
    wait_for_edit(&mut rt);
    let lowest = rt.state.edit.output_bytes().unwrap();
    rt.update(|s, _| s.set_quality(10, true));
    wait_for_edit(&mut rt);
    assert!(rt.state.edit.output_bytes().unwrap() > lowest);
    click(&mut rt, "edit-foot/Reset");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.edit.output_bytes(), None);
    assert_eq!(rt.state.audio_edit(), crate::edit::AudioEdit::default());
    assert_eq!(drag_out(&mut rt, "drag-card")[0].file_name().unwrap(), "tone.wav", "back to a plain copy");

    assert_eq!(snapshot_tree(&root), before, "the library is byte for byte what it was");
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compressing_a_texture_previews_the_output_and_drags_a_png() {
    let root = edit_fixture();
    let before = snapshot_tree(&root);
    let mut rt = app_on(&root);
    open_editor(&mut rt, "noise.jpg");
    click(&mut rt, "colours/16");
    wait_for_edit(&mut rt);
    let distinct = |rt: &Runtime<App>| match &rt.state.stage.scene {
        crate::stage::Scene::Texture(image) => image.rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect::<std::collections::HashSet<_>>().len(),
        _ => panic!("no texture on stage"),
    };
    assert!(distinct(&rt) <= 16, "the preview shows the compressed pixels: {}", distinct(&rt));

    // Original flips back without forgetting the edit.
    click(&mut rt, "compare/Original");
    wait_for_preview(&mut rt);
    assert!(distinct(&rt) > 16);
    click(&mut rt, "compare/Edited");
    wait_for_edit(&mut rt);
    assert!(distinct(&rt) <= 16);

    let staged = drag_out(&mut rt, "drag-card");
    assert_eq!(staged[0].file_name().unwrap(), "noise.png");
    assert_eq!(std::fs::metadata(&staged[0]).unwrap().len(), rt.state.edit.output_bytes().unwrap(), "the drag is the file the panel measured");
    assert_eq!(snapshot_tree(&root), before);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn material_edits_show_on_the_stage_and_leave_as_a_glb() {
    let root = edit_fixture();
    let before = snapshot_tree(&root);
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    assert!(rt.state.stage.surface.is_some(), "the editor shades the model's materials");
    assert_eq!(rt.state.edit.expanded, None, "materials start folded");
    click(&mut rt, "material-0");
    assert_eq!(rt.state.edit.expanded, Some(0));
    for _ in 0..40 { rt.frame(1.0 / 60.0); }

    // A slider moves the stage straight away, before any render.
    let (x, y) = rt.find("m0-rough").unwrap();
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(&mut rt);
    let roughness = rt.state.stage.surface.as_ref().unwrap().materials[0].roughness;
    assert!((roughness - 0.5).abs() < 0.03, "{roughness}");
    rt.pointer_up(x, y);
    wait_for_edit(&mut rt);

    // Shrinking textures reaches the stage too.
    click(&mut rt, "model-tab/Whole model");
    click(&mut rt, "all-textures/64");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.stage.surface.as_ref().unwrap().images[0].width, 64);

    // It leaves as one self-contained GLB, even dragged from the grid.
    let tile = tile_key(&rt, "tri.obj");
    assert!(rt.elements().iter().any(|e| e.path.contains(&tile) && e.path.ends_with("edited")), "the tile says it's edited");
    let staged = drag_out(&mut rt, &tile);
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].file_name().unwrap(), "tri.glb");
    let doc = crate::edit::mesh::open(&staged[0], &root).unwrap();
    assert!((doc.materials[0].roughness - 0.5).abs() < 0.03);
    assert_eq!(doc.image_size(0), Some((64, 64)));

    assert_eq!(snapshot_tree(&root), before);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

fn rect(rt: &Runtime<App>, key: &str) -> [f32; 4] {
    rt.presented(key).unwrap_or_else(|| panic!("no {key}")).0
}

/// Presses without letting the frames pass, so the animation that follows can be watched.
fn press(rt: &mut Runtime<App>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
}

/// Runs `frames` frames, returning the height of `key` after each.
fn heights(rt: &mut Runtime<App>, key: &str, frames: usize, mut check: impl FnMut(&Runtime<App>)) -> Vec<f32> {
    (0..frames).map(|_| {
        rt.frame(1.0 / 60.0);
        check(rt);
        rect(rt, key)[3]
    }).collect()
}

fn eases_between(seen: &[f32], from: f32, to: f32) -> bool {
    let (low, high) = (from.min(to) + 1.0, from.max(to) - 1.0);
    seen.iter().any(|h| (low..high).contains(h))
}

#[test]
fn opening_edit_moves_only_the_preview_and_it_eases() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    let tile = tile_key(&rt, "noise.jpg");
    click(&mut rt, &tile);
    wait_for_preview(&mut rt);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    let drag_bar = rect(&rt, "drag-card");
    let details = rect(&rt, "details");
    let closed = rect(&rt, "stage")[3];

    // Nothing below the preview moves on any frame; the preview shrinks through the steps between.
    press(&mut rt, "edit-toggle");
    let steps = heights(&mut rt, "stage", 40, |rt| {
        assert_eq!(rect(rt, "drag-card"), drag_bar, "the drag bar stays put");
        assert_eq!(rect(rt, "details"), details, "the details and readout share a place");
    });
    wait_for_edit(&mut rt);
    let open = rect(&rt, "stage")[3];
    assert!(open < closed - 100.0, "{open} from {closed}");
    assert!(eases_between(&steps, closed, open), "{steps:?} from {closed} to {open}");

    // Rendering leaves every rectangle where it was, and the readout keeps its bars up.
    let panel = rect(&rt, "edit-panel");
    let foot = rect(&rt, "edit-foot");
    click(&mut rt, "colours/16");
    assert!(rt.find("output-bar").is_some());
    let start = Instant::now();
    while rt.state.edit.busy() && start.elapsed() < Duration::from_secs(10) {
        rt.update(|s, tx| { s.poll(tx); });
        rt.frame(1.0 / 60.0);
        assert_eq!((rect(&rt, "edit-panel"), rect(&rt, "edit-foot"), rect(&rt, "stage")[3]), (panel, foot, open));
        assert!(rt.find("output-bar").is_some(), "the bars stay while rendering");
        std::thread::sleep(Duration::from_millis(2));
    }
    wait_for_edit(&mut rt);
    let first_size = rt.state.edit.output_bytes().unwrap();
    click(&mut rt, "colours/64");
    assert_eq!(rt.state.edit.output_bytes(), Some(first_size), "the last size stays up until the next is ready");
    wait_for_edit(&mut rt);
    for key in ["Dither", "edit-foot/Reset"] { assert!(rt.find(key).is_some(), "{key} is always there"); }
    assert_eq!((rect(&rt, "edit-panel"), rect(&rt, "stage")[3]), (panel, open));

    // Closing gives the room back the same way.
    press(&mut rt, "edit-toggle");
    let steps = heights(&mut rt, "stage", 40, |rt| assert_eq!(rect(rt, "drag-card"), drag_bar));
    assert!(eases_between(&steps, open, closed), "{steps:?}");
    assert_eq!(rect(&rt, "stage")[3], closed);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audio_tabs_and_the_loop_switch_never_resize_the_panel() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let stage = rect(&rt, "waveform");
    let panel = rect(&rt, "edit-panel");
    let holds = |rt: &Runtime<App>| assert_eq!((rect(rt, "waveform"), rect(rt, "edit-panel")), (stage, panel));
    for key in ["audio-tab/Loop", "Loop", "audio-tab/Compress", "audio-tab/Trim", "audio-tab/Loop", "Loop"] {
        press(&mut rt, key);
        for _ in 0..30 { rt.frame(1.0 / 60.0); holds(&rt); }
        wait_for_edit(&mut rt);
        holds(&rt);
    }
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn flipping_to_the_original_and_back_keeps_a_models_edits_on_stage() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    rt.update(|s, _| s.set_material(0, true, |m| m.roughness = Some(0.2)));
    wait_for_edit(&mut rt);
    click(&mut rt, "model-tab/Whole model");
    click(&mut rt, "all-textures/64");
    wait_for_edit(&mut rt);
    let shown = |rt: &Runtime<App>| {
        let surface = rt.state.stage.surface.as_ref().unwrap();
        (surface.materials[0].roughness, surface.images[0].width)
    };
    assert_eq!(shown(&rt), (0.2, 64));

    click(&mut rt, "compare/Original");
    wait_for_edit(&mut rt);
    assert_eq!(shown(&rt), (1.0, 128), "Original shows the source's values and textures");

    click(&mut rt, "compare/Edited");
    wait_for_edit(&mut rt);
    assert_eq!(shown(&rt), (0.2, 64), "the edit comes back as it was, with nothing else touched");
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_render_finishing_while_comparing_is_there_when_flipping_back() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    click(&mut rt, "model-tab/Whole model");
    click(&mut rt, "all-textures/64");
    click(&mut rt, "compare/Original");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.stage.surface.as_ref().unwrap().images[0].width, 128);
    click(&mut rt, "compare/Edited");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.stage.surface.as_ref().unwrap().images[0].width, 64);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn material_rows_show_a_ball_presets_and_their_maps() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    click(&mut rt, "material-0");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(rt.find("ball-0").is_some());
    assert_eq!(rt.state.edit.balls[0].size(), (crate::edit::ball::BALL_PX, crate::edit::ball::BALL_PX), "the ball was drawn");
    assert_eq!(rt.state.edit.map_thumbs[0].size(), (128, 128), "the map strip has the texture");

    // A preset sets both values, and the ball follows.
    click(&mut rt, "m0-preset/Metal");
    wait_for_edit(&mut rt);
    let edit = rt.state.model_edit();
    assert_eq!((edit.materials[&0].roughness, edit.materials[&0].metallic), (Some(0.3), Some(1.0)));
    assert_eq!(rt.state.edit.drawn_ball(0).map(|m| (m.roughness, m.metallic)), Some((0.3, 1.0)), "the ball was redrawn");

    // Hovering a row picks the material out on the stage, and leaving lets it go.
    rt.update(|s, _| s.set_material_focus(0, true));
    assert_eq!(rt.state.stage.surface.as_ref().unwrap().focus, Some(0));
    rt.update(|s, _| s.set_material_focus(0, false));
    assert_eq!(rt.state.stage.surface.as_ref().unwrap().focus, None);

    // Folding the open row back leaves a small ball.
    let open = rect(&rt, "ball-0")[2];
    click(&mut rt, "ball-0");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(rect(&rt, "ball-0")[2] < open);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn texture_filters_reach_the_stage_and_the_exported_file() {
    use crate::edit::mesh::TextureFilter::{Pixels, Smooth};
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    let base_filter = |rt: &Runtime<App>| rt.state.stage.surface.as_ref().unwrap().materials[0].filters[0];
    assert_eq!(base_filter(&rt), None, "an OBJ doesn't say");

    // The whole model, in one click.
    click(&mut rt, "model-tab/Whole model");
    click(&mut rt, "model-filter/Pixels");
    wait_for_edit(&mut rt);
    assert_eq!(base_filter(&rt), Some(Pixels));

    // One texture can differ, from the thumbnail that shows it.
    click(&mut rt, "model-tab/Materials");
    click(&mut rt, "material-0");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "map-0-Base");
    assert!(rt.popup_menu().is_some(), "the thumbnail opens a menu");
    rt.resolve_popup(Some("smooth")).unwrap();
    wait_for_edit(&mut rt);
    assert_eq!(base_filter(&rt), Some(Smooth), "a texture's own filter wins");

    // The file carries what's on screen.
    let tile = tile_key(&rt, "tri.obj");
    let staged = drag_out(&mut rt, &tile);
    let doc = crate::edit::mesh::open(&staged[0], &root).unwrap();
    assert_eq!(doc.file_filter(0), Some(Smooth));

    // Choosing for the whole model again takes over, and Original shows the file's.
    click(&mut rt, "model-tab/Whole model");
    click(&mut rt, "model-filter/Smooth");
    wait_for_edit(&mut rt);
    assert!(rt.state.model_edit().textures.is_empty());
    click(&mut rt, "compare/Original");
    wait_for_edit(&mut rt);
    assert_eq!(base_filter(&rt), None);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

/// A pointer drag over `key`, from `from` to `to` as shares of its width, at its middle height.
fn drag_across(rt: &mut Runtime<App>, key: &str, from: f32, to: f32) {
    let box_ = rect(rt, key);
    let (y, at) = (box_[1] + box_[3] * 0.5, |share: f32| box_[0] + box_[2] * share);
    rt.pointer_move(at(from), y);
    rt.pointer_down(at(from), y);
    settle(rt);
    rt.pointer_move(at(to), y);
    settle(rt);
    rt.pointer_up(at(to), y);
    settle(rt);
}

#[test]
fn dragging_the_waveform_picks_the_section_and_clicking_only_seeks() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }

    drag_across(&mut rt, "waveform", 0.25, 0.5);
    wait_for_edit(&mut rt);
    let (start, end, frames) = rt.state.audio_section().unwrap();
    let share = |frame: u64| frame as f32 / frames as f32;
    assert!((share(start) - 0.25).abs() < 0.02 && (share(end) - 0.5).abs() < 0.02, "{start}..{end} of {frames}");

    // Backwards works too.
    drag_across(&mut rt, "waveform", 0.8, 0.6);
    wait_for_edit(&mut rt);
    let (start, end, _) = rt.state.audio_section().unwrap();
    assert!((share(start) - 0.6).abs() < 0.02 && (share(end) - 0.8).abs() < 0.02, "{start}..{end}");

    // A click doesn't touch the section.
    let picked = rt.state.audio_section();
    let box_ = rect(&rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.7, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.audio_section(), picked);

    // Outside the editor the waveform goes back to plain scrubbing.
    click(&mut rt, "edit-toggle");
    for _ in 0..80 { rt.frame(1.0 / 60.0); }
    drag_across(&mut rt, "waveform", 0.1, 0.3);
    assert_eq!(rt.state.audio_section(), picked, "browsing can't change the edit");
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_timeline_zooms_about_the_pointer_and_selection_follows_the_zoom() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(rt.state.edit.view.is_none() && rt.find("timeline-overview").is_none());
    let panel = rect(&rt, "edit-panel");

    // Zooming keeps what's under the pointer where it is.
    let box_ = rect(&rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.5, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    for _ in 0..6 {
        rt.wheel_event(rune::WheelEvent::new(0.0, 80.0).at(x, y));
        rt.frame(1.0 / 60.0);
    }
    wait_for_edit(&mut rt);
    let (start, end) = rt.state.edit.view.expect("the wheel zoomed in");
    let frames = rt.state.audio_section().unwrap().2;
    assert!((end - start) < frames / 4, "{start}..{end}");
    assert!((((start + end) / 2) as i64 - (frames / 2) as i64).abs() < 400, "stayed centred: {start}..{end}");
    assert!(rt.find("timeline-overview").is_some());
    assert_eq!(rect(&rt, "edit-panel"), panel, "zooming moves nothing else");

    // Dragging at this zoom measures in the zoomed frames.
    drag_across(&mut rt, "waveform", 0.25, 0.75);
    wait_for_edit(&mut rt);
    let (first, last, _) = rt.state.audio_section().unwrap();
    let quarter = (end - start) / 4;
    assert!((first as i64 - (start + quarter) as i64).abs() < 300, "{first} in {start}..{end}");
    assert!((last as i64 - (end - quarter) as i64).abs() < 300, "{last} in {start}..{end}");

    // Fit brings the whole sound back, and the selection is untouched.
    click(&mut rt, "Fit");
    wait_for_edit(&mut rt);
    assert!(rt.state.edit.view.is_none());
    assert_eq!(rt.state.audio_section().map(|s| (s.0, s.1)), Some((first, last)));
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_zoom_eases_there_and_keeps_the_pointer_on_the_same_moment() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let box_ = rect(&rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.25, box_[1] + box_[3] * 0.5);
    let frames = rt.state.audio_section().unwrap().2 as f64;
    rt.pointer_move(x, y);
    rt.wheel_event(rune::WheelEvent::new(0.0, 200.0).at(x, y));
    rt.frame(1.0 / 60.0);

    // Each step lands somewhere between, with the same moment under the pointer throughout.
    let mut spans = Vec::new();
    let start = Instant::now();
    while rt.state.edit.timeline_moving() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(4));
        rt.update(|s, tx| { s.poll(tx); });
        rt.frame(1.0 / 60.0);
        let (from, to, _) = rt.state.audio_view().unwrap();
        spans.push(to - from);
        let under = from as f64 + (to - from) as f64 * 0.25;
        assert!((under - frames * 0.25).abs() < frames * 0.002, "drifted to {under} of {frames}");
    }
    assert!(spans.len() > 3, "it took more than a jump: {spans:?}");
    assert!(spans.windows(2).all(|w| w[1] <= w[0]), "{spans:?}");
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

/// Zooms the open timeline in on the middle of its width and waits for it to arrive.
fn zoom_in(rt: &mut Runtime<App>, notches: usize) {
    let box_ = rect(rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.5, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    for _ in 0..notches {
        rt.wheel_event(rune::WheelEvent::new(0.0, 80.0).at(x, y));
        rt.frame(1.0 / 60.0);
    }
    wait_for_edit(rt);
}

#[test]
fn zooming_past_the_playhead_is_not_pulled_back_to_it() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    zoom_in(&mut rt, 1);

    // The playhead sits inside the view, then a zoom about the far right leaves it behind.
    let (start, _) = rt.state.edit.view.unwrap();
    let playhead = (start + 2_000) as f32 / 44_100.0;
    rt.update(move |s, _| { s.follow_playhead_while(true, playhead); });
    let box_ = rect(&rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.9, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    for _ in 0..8 { rt.wheel_event(rune::WheelEvent::new(0.0, 80.0).at(x, y)); }
    rt.frame(1.0 / 60.0);

    // Ticking through the whole ease, playback never gets to redirect it.
    let began = Instant::now();
    while rt.state.edit.timeline_moving() && began.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(4));
        let mut followed = false;
        rt.update(|s, tx| {
            s.poll(tx);
            followed = s.follow_playhead_while(true, playhead);
        });
        assert!(!followed, "the zoom was overruled");
        rt.frame(1.0 / 60.0);
    }
    let (first, last) = rt.state.edit.view.unwrap();
    let frame = (playhead * 44_100.0) as u64;
    assert!(!(first..last).contains(&frame), "the zoom went past the playhead: {first}..{last} at {frame}");
    let mut followed = false;
    rt.update(|s, _| followed = s.follow_playhead_while(true, playhead));
    assert!(!followed, "and it stays there once it has stopped");

    // Playback itself running off the edge still pages along.
    let inside = (first + 10) as f32 / 44_100.0;
    let beyond = (last + 10) as f32 / 44_100.0;
    rt.update(move |s, _| { s.follow_playhead_while(true, inside); });
    let mut paged = false;
    rt.update(|s, _| paged = s.follow_playhead_while(true, beyond));
    assert!(paged);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_middle_button_pans_the_timeline_and_nothing_else() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    zoom_in(&mut rt, 6);
    let (start, end) = rt.state.edit.view.unwrap();
    let section = rt.state.audio_section();

    // Dragging right pulls the sound right, so the view moves toward the start.
    let box_ = rect(&rt, "waveform");
    let (x, y, width) = (box_[0] + box_[2] * 0.5, box_[1] + box_[3] * 0.5, box_[2]);
    rt.pointer_move(x, y);
    rt.button_down(x, y, rune::PointerButton::Middle);
    settle(&mut rt);
    rt.pointer_move(x + width * 0.25, y);
    settle(&mut rt);
    rt.button_up(x + width * 0.25, y, rune::PointerButton::Middle);
    settle(&mut rt);
    let (moved_start, moved_end) = rt.state.edit.view.unwrap();
    let span = end - start;
    assert_eq!(moved_end - moved_start, span, "panning doesn't zoom");
    assert!(((start - moved_start) as i64 - (span / 4) as i64).abs() < 300, "moved {} of {span}", start - moved_start);
    assert_eq!(rt.state.audio_section(), section, "and it selects nothing");
    assert!(!rt.state.edit.is_edited(rt.state.selected_asset().unwrap().key));
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zooming_works_without_opening_the_editor() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    let tile = tile_key(&rt, "tone.wav");
    click(&mut rt, &tile);
    wait_for_preview(&mut rt);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    assert!(!rt.state.edit.open);

    let box_ = rect(&rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * 0.5, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    rt.wheel_event(rune::WheelEvent::new(0.0, 200.0).at(x, y));
    rt.frame(1.0 / 60.0);
    wait_for_edit(&mut rt);
    let (start, end) = rt.state.edit.view.expect("the first scroll reads the sound and still zooms");
    assert!(rt.find("timeline-overview").is_some() && rt.find("Fit").is_some());
    assert!(rt.find("mark-end").is_none(), "no editing controls appear");

    // Scrubbing goes by the zoomed view, and doesn't start an edit.
    drag_across(&mut rt, "waveform", 0.5, 0.5);
    assert!(!rt.state.edit.is_edited(rt.state.selected_asset().unwrap().key));
    assert_eq!(rt.state.edit.view, Some((start, end)));

    click(&mut rt, "Fit");
    wait_for_edit(&mut rt);
    assert!(rt.state.edit.view.is_none());
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

fn key_press(rt: &mut Runtime<App>, key: Key, shift: bool) {
    rt.key(KeyEvent { key, mods: rune::Mods { shift, ..Default::default() } });
    settle(rt);
}

fn space(rt: &mut Runtime<App>) {
    key_press(rt, Key::Text(" ".into()), false);
}

/// The tone in the editor, not autoplaying, with keys wired up and the pointer on the sound.
fn transport_on_tone(root: &Path) -> Runtime<App> {
    let mut rt = app_on(root);
    rt.state.autoplay = false;
    rt.on_key = Some(ui::on_key);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let (x, y) = rt.find("waveform").unwrap();
    rt.pointer_move(x, y);
    settle(&mut rt);
    rt
}

fn click_waveform_at(rt: &mut Runtime<App>, share: f32) {
    let box_ = rect(rt, "waveform");
    let (x, y) = (box_[0] + box_[2] * share, box_[1] + box_[3] * 0.5);
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(rt);
}

#[test]
fn space_plays_from_the_cue_and_stops_back_at_it() {
    let root = edit_fixture();
    let mut rt = transport_on_tone(&root);
    assert!(rt.state.audio.is_finished());

    // A click only places the cue. The sound stays quiet.
    click_waveform_at(&mut rt, 0.5);
    assert!(rt.state.audio.is_finished(), "clicking doesn't start playback");
    assert!((rt.state.cue_point() - 1.0).abs() < 0.05, "{}", rt.state.cue_point());

    space(&mut rt);
    assert!(rt.state.audio.is_playing());
    space(&mut rt);
    assert!(rt.state.audio.is_finished(), "Space again stops");
    assert!((rt.state.cue_point() - 1.0).abs() < 0.05, "and it's still where it started");

    // A finished sound comes back to the cue too.
    space(&mut rt);
    rt.state.audio.finish_silently();
    rt.update(|s, tx| { s.poll(tx); });
    settle(&mut rt);
    assert!(rt.state.audio.is_finished());
    assert!((rt.state.cue_point() - 1.0).abs() < 0.05);

    // Clicking while it plays moves both the sound and the cue, and keeps playing.
    space(&mut rt);
    click_waveform_at(&mut rt, 0.25);
    assert!(rt.state.audio.is_playing());
    assert!((rt.state.cue_point() - 0.5).abs() < 0.05);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shift_space_pauses_and_home_goes_back_to_the_start() {
    let root = edit_fixture();
    let mut rt = transport_on_tone(&root);
    click_waveform_at(&mut rt, 0.5);
    space(&mut rt);

    key_press(&mut rt, Key::Text(" ".into()), true);
    assert!(!rt.state.audio.is_playing() && !rt.state.audio.is_finished(), "paused, still under way");
    key_press(&mut rt, Key::Text(" ".into()), true);
    assert!(rt.state.audio.is_playing(), "and it carries on");

    // Home forgets the clicked point, without interrupting.
    key_press(&mut rt, Key::Home, false);
    assert!(rt.state.audio.is_playing());
    assert_eq!(rt.state.cue_point(), 0.0);

    // The button does the same as the key.
    click_waveform_at(&mut rt, 0.5);
    assert!(rt.state.cue_point() > 0.5);
    click(&mut rt, "rewind");
    assert_eq!(rt.state.cue_point(), 0.0);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cue_follows_the_section() {
    let root = edit_fixture();
    let mut rt = transport_on_tone(&root);

    // A new selection is where playback starts.
    click_waveform_at(&mut rt, 0.1);
    drag_across(&mut rt, "waveform", 0.5, 0.75);
    wait_for_edit(&mut rt);
    let (start, _, _) = rt.state.audio_section().unwrap();
    assert!((rt.state.cue_point() - start as f32 / 44_100.0).abs() < 0.001, "{}", rt.state.cue_point());

    // Inside it a click counts, and outside it falls back to the section's start.
    click_waveform_at(&mut rt, 0.6);
    assert!(rt.state.cue_point() > start as f32 / 44_100.0 + 0.05);
    click_waveform_at(&mut rt, 0.9);
    assert!((rt.state.cue_point() - start as f32 / 44_100.0).abs() < 0.001);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stopping_brings_a_zoomed_timeline_back_to_the_cue() {
    let root = edit_fixture();
    let mut rt = transport_on_tone(&root);
    zoom_in(&mut rt, 8);
    let (x, _) = rt.state.edit.view.unwrap();
    assert!(x > 10_000, "the view left the start behind: {x}");
    space(&mut rt);
    wait_for_edit(&mut rt);
    let (start, _) = rt.state.edit.view.unwrap();
    assert!(start < 1_000, "starting at the beginning brings it into view: {start}");
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn space_plays_the_sound_while_the_pointer_is_over_it() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    rt.state.autoplay = false;
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    assert!(rt.state.edit.open, "the click left focus on the Edit button");
    rt.on_key = Some(ui::on_key);
    let space = || KeyEvent { key: Key::Text(" ".into()), mods: Default::default() };
    let (x, y) = rt.find("waveform").unwrap();

    // From a focused button outside, moving onto the sound hands Space to it.
    rt.pointer_move(x, y);
    settle(&mut rt);
    rt.key(space());
    settle(&mut rt);
    assert!(rt.state.edit.open, "the Edit button didn't get the key");
    assert!(rt.state.edit.timeline.is_some(), "playback started");

    // Pressing a control inside the sound area doesn't take it back.
    let autoplay = rt.state.autoplay;
    click(&mut rt, "Play on select");
    assert_ne!(rt.state.autoplay, autoplay);
    rt.pointer_move(x, y);
    settle(&mut rt);
    rt.key(space());
    settle(&mut rt);
    assert_ne!(rt.state.autoplay, autoplay, "Space went to the sound, not the checkbox");

    // Away from the sound, a focused control keeps its usual Space.
    click(&mut rt, "audio-tab/Loop");
    let (away_x, away_y) = rt.find("edit-foot").unwrap();
    rt.pointer_move(away_x, away_y);
    settle(&mut rt);
    assert_eq!(rt.state.edit.audio_tab, crate::editor::AudioTab::Loop);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

/// Drags element `key` by a number of points, pressing at its centre.
fn drag_by(rt: &mut Runtime<App>, key: &str, dx: f32, dy: f32) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    settle(rt);
    rt.pointer_move(x + dx, y + dy);
    settle(rt);
    rt.pointer_up(x + dx, y + dy);
    settle(rt);
}

#[test]
fn fades_are_set_by_dragging_handles_on_the_waveform() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    rt.state.autoplay = false;
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let width = rect(&rt, "waveform")[2];
    let two_seconds_per_width = 2000.0 / width;

    // Both handles are waiting at the top corners, before any fade exists.
    assert!(rt.find("fade-in-grip").is_some() && rt.find("fade-out-grip").is_some());
    assert!(rt.find("fade-in-shape").is_none() && rt.find("fade-curves").is_none(), "no curve until there's a fade");

    // Dragging in sets the length, with no effect on the section.
    let section = rt.state.audio_section();
    drag_by(&mut rt, "fade-in-grip", 60.0, 0.0);
    wait_for_edit(&mut rt);
    let expected = 60.0 * two_seconds_per_width;
    assert!((rt.state.audio_edit().fade_in_ms as f32 - expected).abs() < 3.0 * two_seconds_per_width, "{} of {expected}", rt.state.audio_edit().fade_in_ms);
    assert_eq!(rt.state.audio_section(), section);
    assert!(rt.find("fade-curves").is_some());

    // The out handle goes the other way.
    drag_by(&mut rt, "fade-out-grip", -80.0, 0.0);
    wait_for_edit(&mut rt);
    assert!(rt.state.audio_edit().fade_out_ms > 0);

    // The dot bends the curve: up comes up earlier, and a double click straightens it.
    drag_by(&mut rt, "fade-in-shape", 0.0, -30.0);
    wait_for_edit(&mut rt);
    assert!(rt.state.audio_edit().fade_in_shape < -0.2, "{}", rt.state.audio_edit().fade_in_shape);
    let (x, y) = rt.find("fade-in-shape").unwrap();
    rt.pointer_move(x, y);
    for _ in 0..2 {
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
    }
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.audio_edit().fade_in_shape, 0.0);

    // A fade can't run past the other one, however far it's dragged.
    let out = rt.state.audio_edit().fade_out_ms;
    drag_by(&mut rt, "fade-in-grip", width * 2.0, 0.0);
    wait_for_edit(&mut rt);
    assert!(rt.state.audio_edit().fade_in_ms + out <= 2000, "{} + {out}", rt.state.audio_edit().fade_in_ms);

    // Level shows it, and clears it.
    click(&mut rt, "audio-tab/Level");
    click(&mut rt, "clear-Fade out");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.audio_edit().fade_out_ms, 0);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_loop_has_no_fade_handles() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    rt.state.autoplay = false;
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    rt.update(|s, _| s.set_looped(true));
    wait_for_edit(&mut rt);
    assert!(rt.find("fade-in-grip").is_none());
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn level_fades_and_normalize_reach_the_exported_sound() {
    let root = edit_fixture();
    let before = snapshot_tree(&root);
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tone.wav");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let panel = rect(&rt, "edit-panel");

    click(&mut rt, "audio-tab/Level");
    assert_eq!(rect(&rt, "edit-panel"), panel, "the new tab fits the same panel");
    let panel_right = panel[0] + panel[2];
    for key in ["Normalize", "clear-Fade in", "clear-Fade out"] {
        let control = rect(&rt, key);
        assert!(control[0] + control[2] <= panel_right, "{key} runs past the panel: {control:?} in {panel:?}");
    }
    // At the narrowest the inspector goes, too.
    rt.update(|s, _| s.resize_inspector(300.0));
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let narrow = rect(&rt, "edit-panel");
    let control = rect(&rt, "Normalize");
    assert!(control[0] + control[2] <= narrow[0] + narrow[2], "{control:?} in {narrow:?}");
    rt.update(|s, _| s.resize_inspector(372.0));
    for _ in 0..40 { rt.frame(1.0 / 60.0); }

    click(&mut rt, "Normalize");
    wait_for_edit(&mut rt);
    assert!((rt.state.edit.peak_db.unwrap() - crate::edit::sound::NORMALIZE_PEAK_DB).abs() < 0.05, "{:?}", rt.state.edit.peak_db);
    rt.update(|s, _| s.set_audio_edit(crate::edit::AudioEdit { fade_out_ms: 200, ..s.audio_edit() }));
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.audio_edit().fade_out_ms, 200);

    let staged = drag_out(&mut rt, "drag-card");
    let heard = crate::edit::sound::decode_file(&staged[0]).unwrap();
    let frames = heard.samples.len();
    let loudest = |slice: &[f32]| slice.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(loudest(&heard.samples[frames / 2..frames / 2 + 4000]) > 0.8, "normalized up");
    assert!(loudest(&heard.samples[frames - 40..]) < 0.1, "faded out");

    // A loop has no ends to fade, so those controls stand down without moving anything.
    click(&mut rt, "audio-tab/Loop");
    click(&mut rt, "Loop");
    wait_for_edit(&mut rt);
    assert_eq!(rect(&rt, "edit-panel"), panel);
    assert_eq!(snapshot_tree(&root), before);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_whole_model_tab_moves_the_origin_and_scales_the_export() {
    let root = edit_fixture();
    let before = snapshot_tree(&root);
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    let marker = |rt: &Runtime<App>| rt.state.stage.surface.as_ref().unwrap().origin;
    assert_eq!(marker(&rt), None, "the marker belongs to the Whole model tab");

    click(&mut rt, "model-tab/Whole model");
    assert_eq!(marker(&rt), Some([0.0; 3]), "it starts where the file puts it");
    click(&mut rt, "model-origin/Bottom");
    wait_for_edit(&mut rt);
    assert_eq!(marker(&rt), Some([0.5, 0.0, 0.0]));
    click(&mut rt, "scale-preset/\u{00f7}100");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.model_edit().scale, Some(0.01));
    assert_eq!(rt.state.edit.scale_text.text, "0.01", "the field follows the preset");

    // Typing wins, and half-typed text does nothing until it's a number.
    rt.update(|s, _| {
        s.edit.scale_text.replace_text("2.".to_owned());
        s.scale_typed();
        s.edit.scale_text.replace_text("oops".to_owned());
        s.scale_typed();
    });
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.model_edit().scale, Some(2.0));

    let staged = drag_out(&mut rt, "drag-card");
    let exported = crate::model::load(&staged[0], &root).unwrap();
    let lowest = exported.vertices.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
    let widest = exported.vertices.iter().map(|v| v.pos[0]).fold(f32::MIN, f32::max);
    assert_eq!((lowest, widest), (0.0, 1.0), "based at the origin, twice as wide either side of it");

    // Reset clears the field as well as the edit.
    click(&mut rt, "edit-foot/Reset");
    wait_for_edit(&mut rt);
    assert_eq!(rt.state.edit.scale_text.text, "1");
    assert_eq!(snapshot_tree(&root), before);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_material_row_springs_open_and_closed_inside_a_fixed_panel() {
    let root = edit_fixture();
    let mut rt = app_on(&root);
    open_editor(&mut rt, "tri.obj");
    click(&mut rt, "material-0");
    for _ in 0..40 { rt.frame(1.0 / 60.0); }
    let panel = rect(&rt, "edit-panel");
    let stage = rect(&rt, "stage")[3];
    let open = rect(&rt, "fold-0")[3];
    assert!(open > 100.0, "{open}");
    press(&mut rt, "ball-0");
    let steps = heights(&mut rt, "fold-0", 40, |rt| assert_eq!((rect(rt, "edit-panel"), rect(rt, "stage")[3]), (panel, stage)));
    assert!(eases_between(&steps, open, 0.0), "{steps:?}");
    assert_eq!(rect(&rt, "fold-0")[3], 0.0);
    drop(rt);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_list_under_the_search_box_completes_a_query() {
    let files = ["Bricks/brick.png", "Bricks/brick_normal.png", "Props/crate.glb", "Sfx/hit.wav"]
        .map(|rel| library::Entry { rel: rel.into(), size: 1, mtime: 1 }).into_iter().collect();
    let mut state = app::init_with(rune::prefs::Prefs::memory());
    state.lib = Arc::new(library::Library::build(PathBuf::from("Z:/none"), files));
    state.results = library::search(&state.lib, &state.filter);
    state.scanning = false;
    let mut rt = Runtime::new(state, ui::view, (1520.0, 900.0), Env::new(true, 1.0));
    settle(&mut rt);
    let tab = |rt: &mut Runtime<App>, key: Key| { rt.key(KeyEvent { key, mods: Default::default() }); settle(rt); };
    assert!(rt.find("search-suggestions").is_none(), "only while the box is in use");

    // Focused and empty, it lists what can be asked for. Clicking a row keeps the box in use.
    click(&mut rt, "search");
    assert!(rt.find("search-suggestions").is_some());
    click(&mut rt, "suggest-0");
    assert_eq!(rt.state.search.text, "is:");
    assert!(rt.state.search.focused, "typing carries on from the click");

    // Arrows pick a value, Tab takes it, and the search runs.
    tab(&mut rt, Key::Down);
    tab(&mut rt, Key::Down);
    assert_eq!(rt.state.suggestion_at, Some(1));
    tab(&mut rt, Key::Tab);
    assert_eq!(rt.state.search.text, "is:texture ");
    assert_eq!(rt.state.results.ids.len(), 2);
    assert_eq!(rt.state.search.focus, rt.state.search.text.len(), "the caret follows");
    assert!(rt.find("search-suggestions").is_none(), "a finished piece has nothing to add");

    // Fields and words combine with the sidebar's facets.
    rt.update(|s, tx| { s.search = rune::TextEdit::new("is:normal"); s.refilter(tx); });
    settle(&mut rt);
    assert_eq!(rt.state.results.ids.len(), 1);

    // A typo says what it searched for instead.
    rt.update(|s, tx| { s.search = rune::TextEdit::new("cratte"); s.refilter(tx); });
    settle(&mut rt);
    assert_eq!(rt.state.results.corrected.as_deref(), Some("crate"));
    assert!(rt.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.contains("for \u{201c}crate\u{201d}"))));

    // Escape leaves the box and the list goes with it.
    rt.update(|s, _| { s.search = rune::TextEdit::new(""); s.search.focused = true; });
    settle(&mut rt);
    assert!(rt.find("search-suggestions").is_some());
    tab(&mut rt, Key::Escape);
    assert!(rt.find("search-suggestions").is_none());
}
