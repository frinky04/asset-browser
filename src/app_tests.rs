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
    for ext in ["png", "jpg", "jpeg", "bmp", "tga", "webp", "glb", "gltf", "obj", "dae", "wav", "ogg", "mp3", "flac"] {
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
                let (main, toggle, group, tabs) = (rect_of(&rt, "panels/main"), rect_of(&rt, "filters/Every format"), rect_of(&rt, "filters/format-group"), rect_of(&rt, "filters/kinds"));
                let at = format!("window {width}, inspector {inspector}, sidebar {open}, grid {}", rt.state.grid_width);
                assert!(toggle[0] + toggle[2] <= main[0] + main[2] - 15.0, "toggle past the panel at {at}: {toggle:?} in {main:?}");
                assert!(group[0] + group[2] <= toggle[0] - 1.0, "formats touch the toggle at {at}: {group:?}, {toggle:?}");
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

    // A new launch reads what was saved.
    let again = app::init_with(std::mem::replace(&mut rt.state.prefs, rune::prefs::Prefs::memory()));
    assert_eq!(again.theme, app::Theme::Light);
    assert!(!again.autoplay);
    assert!((again.volume - rt.state.volume).abs() < 1e-3);
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

    // Clicking selects; the size ladder offers smaller sizes and never larger.
    let brick = tile_key(&rt, "brick_0.png");
    click(&mut rt, &brick);
    wait_for_preview(&mut rt);
    assert!(matches!(rt.state.preview, Preview::Texture(_)));
    assert!(rt.find("rung-512").is_some() && rt.find("rung-1024").is_none());

    // Dragging a rung stages a downscaled copy and leaves the source alone.
    let source = root.join("Bricks/brick_0.png");
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let (x, y) = rt.find("rung-256").unwrap();
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

    // The tile honours the toolbar's drag size.
    click(&mut rt, "512");
    assert_eq!(rt.state.drag_max, Some(512));
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

    drop(rt);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}
