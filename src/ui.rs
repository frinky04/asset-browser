//! The view. Lifted panels on a near-black window, so the assets are the only colour on
//! screen; white marks what's active, and Inter has tabular figures wherever numbers line up.

use std::sync::LazyLock;

use rune::color::hex;
use rune::{Color, Cursor, Cx, El, GrowthSpec, Icon, Phase, PointerButton, ScrollSpec, SurfaceFit, TextStyle, Tx, col, el, icon, memo, row, text};
use rune_kit::{Kit, PaletteSeed, ScrollBar, TextInput};

use crate::app::{App, GRID_GAP, GRID_PAD, Preview, bytes, clock, commas, ladder};
use crate::library::{Asset, Kind, Sort};
use crate::stage::{FOV_DEGREES, STAGE_KEY, STAGE_MARK, actual_size_zoom, zoom_about};
use crate::thumbs::Facts;

pub struct Tokens {
    bg: Color,
    panel: Color,
    well: Color,
    raised: Color,
    line: Color,
    text: Color,
    muted: Color,
    faint: Color,
    accent: Color,
    on_accent: Color,
    hover: Color,
}

fn color(s: &str) -> Color {
    hex(s).unwrap_or(Color::WHITE)
}

pub static T: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    bg: color("#08080A"),
    panel: color("#131315"),
    well: color("#1C1C1F"),
    raised: color("#2A2A2E"),
    line: color("#FFFFFF14"),
    text: color("#F4F4F5"),
    muted: color("#A6A6AD"),
    faint: color("#6F6F77"),
    accent: color("#F4F4F5"),
    on_accent: color("#0B0B0C"),
    hover: color("#FFFFFF0D"),
});

const PANEL_RADIUS: f32 = 16.0;
const WELL_RADIUS: f32 = 11.0;
const CONTROL_RADIUS: f32 = 9.0;

/// A lifted surface floating on the near-black window.
fn surface(e: El) -> El {
    e.fill(T.panel).radius(PANEL_RADIUS).border(1.0, T.line).shadow(10.0, 28.0, Color::BLACK.alpha(0.45))
}

/// A quiet outlined button.
fn outlined(name: String, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    el().key(name.clone()).role("button").focusable(true).label(name.clone()).px(12.0).h(30.0).radius(CONTROL_RADIUS).center_y()
        .border(1.0, T.line).hover(T.hover).cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(label(name, LABEL, T.text))
}

fn kit() -> &'static Kit {
    static K: LazyLock<KitCell> = LazyLock::new(|| {
        let mut k = Kit::from_palette(PaletteSeed { bg: T.bg, fg: T.text, accent: T.accent });
        k.c.border_focus = T.faint;
        k.radius.md = CONTROL_RADIUS;
        KitCell(k)
    });
    &K.0
}

struct KitCell(Kit);
// SAFETY: built once from plain values and only ever read, on the UI thread.
unsafe impl Send for KitCell {}
// SAFETY: as above.
unsafe impl Sync for KitCell {}

const LABEL: TextStyle = TextStyle::new(13.0, 550.0).lh(17.0);
const BODY: TextStyle = TextStyle::new(13.0, 450.0).lh(17.0);
const SMALL: TextStyle = TextStyle::new(11.5, 450.0).lh(15.0).features("'tnum' 1");
const NUMBERS: TextStyle = TextStyle::new(12.5, 500.0).lh(16.0).features("'tnum' 1");
const TITLE: TextStyle = TextStyle::new(16.0, 650.0).lh(21.0);

const SEARCH: Icon = Icon { name: "ab-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M10.5 10.5L14 14" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"# };
const IMAGE: Icon = Icon { name: "ab-image", view: 16.0, body: r#"<rect x="2" y="3" width="12" height="10" rx="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M2.5 11.5L6 8l2.5 2.5L10.5 8.5l3 3" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"# };
const CUBE: Icon = Icon { name: "ab-cube", view: 16.0, body: r#"<path d="M8 1.8l5.4 3v6.4L8 14.2l-5.4-3V4.8L8 1.8zM2.8 4.9L8 8l5.2-3.1M8 8v6" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"# };
const SOUND: Icon = Icon { name: "ab-sound", view: 16.0, body: r#"<path d="M2.5 6.5v3M5.25 4v8M8 2v12M10.75 4.5v7M13.5 6.5v3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"# };
const PLAY: Icon = Icon { name: "ab-play", view: 16.0, body: r#"<path d="M5 3.2v9.6L13 8 5 3.2z" fill="currentColor"/>"# };
const PAUSE: Icon = Icon { name: "ab-pause", view: 16.0, body: r#"<path d="M4.5 3h2.5v10H4.5zM9 3h2.5v10H9z" fill="currentColor"/>"# };

fn sized(n: usize, one: &str) -> String {
    format!("{} {one}", commas(n))
}

fn label(s: impl Into<String>, style: TextStyle, c: Color) -> El {
    text(s.into()).font(style).color(c)
}

/// A flat segmented control row. `current` picks the raised segment.
fn segments<V: Copy + PartialEq + 'static>(key: &str, items: Vec<(String, V)>, current: V, on: impl Fn(&mut App, V, &mut Tx) + Clone + 'static) -> El {
    let mut bar = row().key(key.to_owned()).radius(CONTROL_RADIUS + 2.0).border(1.0, T.line).padding(2.0).gap(1.0).items_center();
    for (name, value) in items {
        let selected = value == current;
        let on = on.clone();
        bar = bar.child(
            el().role("radio").checked(selected).label(name.clone()).focusable(true).key(name.clone()).px(10.0).h(26.0).radius(CONTROL_RADIUS).center_y()
                .fill(if selected { T.raised } else { Color::TRANSPARENT })
                .hover(if selected { Color::TRANSPARENT } else { T.hover })
                .cursor(Cursor::Pointer)
                .on_press_tx(move |s: &mut App, tx| on(s, value, tx))
                .child(label(name, NUMBERS, if selected { T.text } else { T.muted })),
        );
    }
    bar
}

fn kind_tabs(s: &App) -> El {
    let r = &s.results;
    let items = vec![
        (format!("All {}", commas(r.any_kind as usize)), None),
        (format!("Textures {}", commas(r.by_kind[0] as usize)), Some(Kind::Texture)),
        (format!("Models {}", commas(r.by_kind[1] as usize)), Some(Kind::Model)),
        (format!("Audio {}", commas(r.by_kind[2] as usize)), Some(Kind::Audio)),
    ];
    segments("kinds", items, s.filter.kind, |s, v, tx| s.set_kind(v, tx))
}

fn toolbar(s: &App) -> El {
    let search = TextInput::new(&s.search)
        .placeholder("Search names and folders. Use -word to exclude")
        .label("Search assets")
        .leading_icon(&SEARCH)
        .clear_button()
        .width(None)
        .on_edit(|s: &mut App| &mut s.search)
        .on_change(|s: &mut App, tx| s.refilter(tx))
        .build(kit())
        .key("search");
    row().key("toolbar").w_full().h(56.0).px(GRID_PAD).gap(12.0).items_center()
        .child(el().grow(1.0).min_w(180.0).child(search))
        .child(kind_tabs(s))
}

fn chip(name: String, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    el().key(name.clone()).role("button").focusable(true).label(name.clone()).px(9.0).h(24.0).radius(8.0).center_y()
        .fill(if on { T.raised } else { Color::TRANSPARENT })
        .hover(if on { Color::TRANSPARENT } else { T.hover })
        .cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(label(name, SMALL, if on { T.text } else { T.muted }))
}

fn filter_bar(s: &App) -> El {
    let mut formats = row().gap(2.0).items_center();
    for (ext, n) in &s.results.by_ext {
        let e: &'static str = ext;
        let name = format!("{e} {}", commas(*n as usize));
        formats = formats.child(chip(name, s.filter.ext == Some(e), move |s, tx| s.set_ext(Some(e), tx)));
    }
    let shadowed = s.filter.all_formats;
    let sorts = vec![
        ("Pack".to_owned(), Sort::Pack), ("Name".to_owned(), Sort::Name), ("Largest".to_owned(), Sort::Largest), ("Newest".to_owned(), Sort::Newest),
    ];
    row().key("filters").w_full().h(36.0).px(GRID_PAD).gap(14.0).items_center()
        .child(el().key("formats").grow(1.0).min_w(0.0).scroll_x().child(formats))
        .child(chip("Every format".to_owned(), shadowed, |s, tx| s.toggle_all_formats(tx)).help("Show every copy of a model when a pack ships it as GLB, OBJ and DAE"))
        .child(row().gap(6.0).items_center().child(label("Sort", SMALL, T.faint)).child(segments("sort", sorts, s.filter.sort, |s, v, tx| s.set_sort(v, tx))))
}

fn sidebar(s: &App) -> El {
    let any = s.results.any_pack as usize;
    let mut list = col().key("packlist").w_full().px(8.0).pb(8.0).gap(2.0);
    let row_for = |name: String, count: usize, on: bool, pack: Option<u32>| {
        let key = name.clone();
        row().key(key).role("button").focusable(true).label(name.clone()).w_full().h(32.0).px(10.0).radius(CONTROL_RADIUS).items_center().gap(8.0)
            .fill(if on { T.raised } else { Color::TRANSPARENT })
            .hover(if on { Color::TRANSPARENT } else { T.hover })
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.set_pack(pack, tx))
            .child(el().grow(1.0).min_w(0.0).child(text(name).font(BODY).color(if count == 0 { T.faint } else if on { T.text } else { T.muted }).truncate()))
            .child(label(commas(count), SMALL, T.faint))
    };
    list = list.child(row_for("All packs".to_owned(), any, s.filter.pack.is_none(), None));
    for (i, pack) in s.lib.packs.iter().enumerate() {
        let i = i as u32;
        list = list.child(row_for(pack.name.clone(), s.results.by_pack.get(i as usize).copied().unwrap_or(0) as usize, s.filter.pack == Some(i), Some(i)));
    }
    let scroller = col().key("packs").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list).scroll_bar(kit());
    surface(col().key("sidebar").w(248.0).h_full().min_h(0.0))
        .child(row().w_full().h(56.0).px(18.0).items_center().child(text(format!("{} packs", s.lib.packs.len())).key("packs-title").font(TITLE).color(T.text)))
        .child(el().w_full().grow(1.0).min_h(0.0).child(scroller))
        .child(el().w_full().padding(8.0).child(
            el().key("change-root").role("button").focusable(true).label("Change library folder").w_full().h(32.0).px(10.0).radius(CONTROL_RADIUS).center_y()
                .hover(T.hover).cursor(Cursor::Pointer)
                .on_press_tx(|_: &mut App, tx: &mut Tx| {
                    tx.open_folder(rune::FileDialogOptions { title: "Choose a library folder".into(), ..Default::default() }, |s: &mut App, picked, _| {
                        if let Ok(Some(path)) = picked { s.change_root(path); }
                    });
                })
                .child(label("Change library folder", BODY, T.muted))))
}

fn facts_line(asset: &Asset, facts: Option<Facts>) -> String {
    match (asset.kind, facts) {
        (_, Some(f)) if f.failed => "Can't preview".to_owned(),
        (Kind::Texture, Some(f)) => format!("{}×{}", f.width, f.height),
        (Kind::Model, Some(f)) => sized(f.width as usize, "tris"),
        (Kind::Audio, Some(f)) => clock(f.seconds),
        _ => String::new(),
    }
}

fn kind_icon(kind: Kind) -> &'static Icon {
    match kind {
        Kind::Texture => &IMAGE,
        Kind::Model => &CUBE,
        Kind::Audio => &SOUND,
    }
}

fn tile(s: &App, id: u32, width: f32, selected: bool) -> El {
    let asset = &s.lib.assets[id as usize];
    let facts = s.thumbs.facts(asset.key);
    let failed = facts.is_some_and(|f| f.failed);
    let px = s.thumbs.pixels(asset.key);
    let mut picture = el().w(width).h(width).radius(WELL_RADIUS).fill(T.well).center().pixels_fit(&px, SurfaceFit::Fit);
    if failed { picture = picture.child(icon(kind_icon(asset.kind), 22.0).color(T.faint)); }
    if selected { picture = picture.outline(2.0, 2.0, T.accent); }
    let max = s.drag_max;
    col().key(format!("a{id}")).role("button").label(asset.name().to_owned()).w(width).gap(TILE_GAP).cursor(Cursor::Grab)
        .opacity(if s.dragging == Some(id) { 0.45 } else { 1.0 })
        .on_press_tx(move |s: &mut App, tx| s.select(id, tx))
        .on_drag(move |s: &mut App, drag, tx| {
            let max = if s.lib.assets[id as usize].kind == Kind::Texture { max } else { None };
            s.drag_gesture(id, max, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(picture)
        .child(col().w_full().gap(1.0)
            .child(text(asset.name().to_owned()).font(LABEL).color(T.text).truncate().w_full())
            .child(row().w_full().justify_between()
                .child(label(facts_line(asset, facts), SMALL, T.muted))
                .child(label(asset.ext.to_owned(), SMALL, T.faint))))
}

fn grid_row(s: &App, r: usize) -> El {
    let cols = s.columns.max(1);
    let ids = &s.results.ids;
    let (from, to) = (r * cols, ((r + 1) * cols).min(ids.len()));
    let width = s.tile_width();
    let selected = s.selected.and_then(|sel| ids[from..to].iter().position(|i| *i == sel));
    let drag = s.dragging.and_then(|d| ids[from..to].iter().position(|i| *i == d));
    let any_facts = ids[from..to].iter().filter(|i| s.thumbs.facts(s.lib.assets[**i as usize].key).is_some()).count();
    let version = (s.generation, cols, width.to_bits(), selected, drag, any_facts, s.drag_max);
    memo(format!("r{r}"), version, || {
        let mut line = row().w_full().h(s.row_height()).px(GRID_PAD).gap(GRID_GAP).items_start();
        for (i, id) in ids[from..to].iter().enumerate() {
            line = line.child(tile(s, *id, width, selected == Some(i)));
        }
        line
    })
}

impl App {
    pub fn row_height(&self) -> f32 {
        // The picture, the gap, the name, the facts, and the space before the next row.
        self.tile_width() + TILE_GAP + 17.0 + 1.0 + 15.0 + 16.0
    }
}

const TILE_GAP: f32 = 7.0;

fn grid(s: &App) -> El {
    let count = s.row_count();
    let rows = col().key("rows").w_full().growth(GrowthSpec::default())
        .virtual_rows(count, s.row_height(), |_: &App, i| format!("r{i}").into(), grid_row)
        .virtual_rows_version((s.generation, s.columns))
        .on_visible_rows(|s: &mut App, range| { s.request_range(range); false });
    let empty = s.results.ids.is_empty() && !s.scanning;
    let body = col().key("grid").w_full().grow(1.0).min_h(0.0).pt(10.0)
        .scroll(ScrollSpec { stick_bottom: false }).child(rows).scroll_bar(kit());
    let mut pane = col().w_full().grow(1.0).min_h(0.0).on_layout(|s: &mut App, r: [f32; 4]| {
        if (r[2] - s.grid_width).abs() > 0.5 { s.set_grid_width(r[2]); }
    });
    if empty && !s.lib.assets.is_empty() {
        pane = pane.center().child(label("Nothing matches. Remove a word or clear the filters.", BODY, T.muted));
    } else if empty {
        pane = pane.center().child(label("This folder has no textures, models or audio. Choose another with Change library folder.", BODY, T.muted));
    } else {
        pane = pane.child(body);
    }
    pane
}

fn status_bar(s: &App) -> El {
    let shown = sized(s.results.ids.len(), "shown");
    let sizes = vec![("S".to_owned(), 120.0f32), ("M".to_owned(), 168.0), ("L".to_owned(), 240.0)];
    let drag_sizes = vec![
        ("Full size".to_owned(), None), ("1024".to_owned(), Some(1024u32)), ("512".to_owned(), Some(512)), ("256".to_owned(), Some(256)),
    ];
    let mut bar = row().key("status").w_full().h(48.0).px(GRID_PAD).gap(16.0).items_center()
        .child(label(shown, SMALL, T.muted))
        .child(text("Making thumbnails: 00,000 of 00,000").live_text(&s.status).font(SMALL).color(T.faint).grow(1.0).min_w(0.0));
    if let Some(m) = &s.message { bar = bar.child(text(m.clone()).key("message").font(SMALL).color(T.muted)); }
    bar.child(row().gap(6.0).items_center().child(label("Drag textures at", SMALL, T.faint)).child(segments("dragmax", drag_sizes, s.drag_max, |s, v, _| s.drag_max = v)))
        .child(segments("tilesize", sizes, s.tile_size, |s, v, _| s.set_tile_size(v)))
}

/// A control in the row under the stage.
fn tool(name: &str, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    chip(name.to_owned(), on, f)
}

fn texture_stage_handlers(stage: El) -> El {
    stage.cursor(Cursor::Grab)
        .pointer_buttons([PointerButton::Middle])
        .on_layout(|s: &mut App, r: [f32; 4]| s.stage_size = [r[2], r[3]])
        .on_drag(|s: &mut App, drag, tx| {
            if drag.phase == Phase::Down { s.drag_last = [drag.dx, drag.dy]; }
            let (dx, dy) = (drag.dx - s.drag_last[0], drag.dy - s.drag_last[1]);
            s.drag_last = [drag.dx, drag.dy];
            s.stage.view.offset[0] += dx;
            s.stage.view.offset[1] += dy;
            tx.live_only();
        })
        .on_wheel(|s: &mut App, e, tx| {
            let ratio = (e.dy * 0.0035).exp();
            let min = if s.stage.view.tile { 0.05 } else { 0.25 };
            let next = (s.stage.view.zoom * ratio).clamp(min, 64.0);
            let at = [e.x - s.stage_size[0] * 0.5, e.y - s.stage_size[1] * 0.5];
            s.stage.view.offset = zoom_about(s.stage.view.offset, at, next / s.stage.view.zoom);
            s.stage.view.zoom = next;
            tx.live_only();
            true
        })
        .on_double_press(|s: &mut App| s.reset_view())
}

fn model_stage_handlers(stage: El) -> El {
    stage.cursor(Cursor::Grab)
        .pointer_buttons([PointerButton::Middle, PointerButton::Secondary])
        .on_layout(|s: &mut App, r: [f32; 4]| s.stage_size = [r[2], r[3]])
        .on_drag(|s: &mut App, drag, tx| {
            if drag.phase == Phase::Down { s.drag_last = [drag.dx, drag.dy]; }
            let (dx, dy) = (drag.dx - s.drag_last[0], drag.dy - s.drag_last[1]);
            s.drag_last = [drag.dx, drag.dy];
            let panning = tx.button() != PointerButton::Primary || tx.mods().shift;
            let view = &mut s.stage.view;
            if panning {
                let per_point = 2.0 * view.distance * (FOV_DEGREES.to_radians() * 0.5).tan() / s.stage_size[1].max(1.0);
                view.pan[0] += dx * per_point;
                view.pan[1] -= dy * per_point;
            } else {
                view.yaw += dx * 0.009;
                view.pitch = (view.pitch + dy * 0.009).clamp(-1.55, 1.55);
            }
            tx.live_only();
        })
        .on_wheel(|s: &mut App, e, tx| {
            s.stage.view.distance = (s.stage.view.distance * (-e.dy * 0.0035).exp()).clamp(1.02, 30.0);
            tx.live_only();
            true
        })
        .on_double_press(|s: &mut App| s.reset_view())
}

fn fact(name: &str, value: String) -> El {
    row().w_full().justify_between().gap(10.0)
        .child(label(name.to_owned(), BODY, T.faint))
        .child(el().min_w(0.0).child(text(value).font(NUMBERS).color(T.text).truncate()))
}

/// A rung of the export ladder: a square sized by the export, ready to be dragged out.
fn rung(id: u32, name: String, max: Option<u32>, side: f32, on: bool) -> El {
    col().key(format!("rung-{name}")).role("button").label(format!("Drag out at {name}")).items_center().justify_end().gap(6.0).h(88.0).cursor(Cursor::Grab)
        .help(format!("Drag out at {name}"))
        .on_drag(move |s: &mut App, drag, tx| {
            s.drag_gesture(id, max, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(el().w(side).h(side).radius(8.0).fill(if on { T.accent } else { T.raised }).hover(if on { Color::TRANSPARENT } else { T.faint }))
        .child(label(name, SMALL, if on { T.text } else { T.muted }))
}

fn texture_export(s: &App, id: u32, width: u32, height: u32) -> El {
    let longest = width.max(height);
    let sizes = ladder(longest);
    let side = |n: u32| 16.0 + 46.0 * (n as f32 / longest as f32).sqrt();
    let full = format!("{width}×{height}");
    let mut steps = row().key("ladder").w_full().gap(10.0).items_end().justify_between()
        .child(rung(id, full, None, side(longest), s.drag_max.is_none()));
    for n in sizes { steps = steps.child(rung(id, n.to_string(), Some(n), side(n), s.drag_max == Some(n))); }
    col().w_full().gap(8.0)
        .child(label("Drag a size out", LABEL, T.text))
        .child(steps)
        .child(row().w_full().gap(4.0).items_center()
            .child(label("Downscale", BODY, T.faint))
            .child(segments("filter", vec![("Smooth".to_owned(), false), ("Hard pixels".to_owned(), true)], s.nearest, |s, v, _| s.nearest = v)))
}

fn drag_card(id: u32, title: &str, detail: String) -> El {
    row().key("drag-card").role("button").label(title.to_owned()).w_full().h(40.0).px(14.0).radius(CONTROL_RADIUS + 2.0).items_center().justify_between().gap(10.0)
        .fill(T.accent).cursor(Cursor::Grab)
        .on_drag(move |s: &mut App, drag, tx| {
            s.drag_gesture(id, None, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(label(title.to_owned(), LABEL, T.on_accent))
        .child(el().min_w(0.0).child(text(detail).font(SMALL).color(T.on_accent.alpha(0.62)).truncate()))
}

fn inspector(s: &App) -> El {
    let shell = surface(col().key("inspector").w(372.0).h_full().min_h(0.0));
    let Some((id, asset)) = s.selected.and_then(|i| s.lib.assets.get(i as usize).map(|a| (i, a))) else {
        return shell.center().child(label("Select an asset to preview it and choose an export size.", BODY, T.faint));
    };
    let facts = s.thumbs.facts(asset.key);
    let mut body = col().w_full().padding(16.0).gap(14.0).min_h(0.0).grow(1.0);

    body = body.child(col().w_full().gap(4.0)
        .child(text(asset.name().to_owned()).key("name").font(TITLE).color(T.text).wrap_anywhere().max_lines(2))
        .child(text(asset.folder().to_owned()).font(SMALL).color(T.faint).truncate_head().w_full()));

    let (tools, stage_body, extra) = match &s.preview {
        Preview::Texture(image) => {
            let view = s.stage.view;
            let size = (image.width, image.height);
            let tools = row().key("tools").w_full().gap(2.0).items_center()
                .child(tool("Fit", view.zoom == 1.0 && !view.tile, |s, _| { s.set_tile(false); s.reset_view(); }))
                .child(tool("1:1", false, move |s, _| {
                    s.stage.view.zoom = actual_size_zoom(s.stage_size, size, s.scale);
                    s.stage.view.offset = [0.0; 2];
                }))
                .child(tool("Tile", view.tile, move |s, _| { let on = !s.stage.view.tile; s.set_tile(on); }))
                .child(tool("Seams", view.seams && view.tile, |s, _| { s.stage.view.seams = !s.stage.view.seams; if s.stage.view.seams && !s.stage.view.tile { s.set_tile(true); } }))
                .child(el().grow(1.0))
                .child(tool("Pixels", !view.smooth, |s, _| s.stage.view.smooth = false))
                .child(tool("Smooth", view.smooth, |s, _| s.stage.view.smooth = true));
            let stage = texture_stage_handlers(el().key(STAGE_KEY.to_owned()).w_full().grow(1.0).min_h(120.0).effect(STAGE_MARK));
            (Some(tools), stage, Some(texture_export(s, id, image.width, image.height)))
        }
        Preview::Model(m) => {
            let tools = row().key("tools").w_full().gap(8.0).items_center()
                .child(tool("Reset view", false, |s, _| s.reset_view()))
                .child(label("Drag to orbit. Right-drag to pan. Scroll to zoom.", SMALL, T.faint));
            let stage = model_stage_handlers(el().key(STAGE_KEY.to_owned()).w_full().grow(1.0).min_h(120.0).effect(STAGE_MARK));
            let tris = sized(m.triangles(), "triangles");
            let images = if m.images.is_empty() { "no textures".to_owned() } else { sized(m.images.len(), if m.images.len() == 1 { "texture" } else { "textures" }) };
            let mut extra = col().w_full().gap(8.0).child(drag_card(id, "Drag model out", format!("{tris}, {images}")));
            if matches!(asset.ext, "obj" | "dae") {
                extra = extra.child(chip("Bring materials and textures along".to_owned(), s.companions, |s, _| s.companions = !s.companions));
            }
            (Some(tools), stage, Some(extra))
        }
        Preview::Audio { seconds, rate, channels } => {
            let seconds = *seconds;
            let playing = s.audio.is_playing();
            let wave = el().key("waveform").w_full().h(240.0).fill(T.well).cursor(Cursor::Pointer)
                .pixels(&s.wave)
                .on_layout(|s: &mut App, r: [f32; 4]| s.stage_size = [r[2], r[3]])
                .on_drag(|s: &mut App, drag, tx| {
                    if drag.phase != Phase::Up { s.seek_fraction(drag.x / drag.w.max(1.0), tx); }
                })
                .child(el().key("playhead").w(1.5).h_full().fill(T.accent).absolute(0.0, 0.0).live_offset(&s.playhead));
            let tools = row().key("tools").w_full().gap(8.0).items_center()
                .child(el().key("play").role("button").label(if playing { "Pause" } else { "Play" }).focusable(true).size(32.0, 32.0).radius(16.0).center().fill(T.accent).cursor(Cursor::Pointer)
                    .on_press(|s: &mut App| s.toggle_playback())
                    .child(icon(if playing { &PAUSE } else { &PLAY }, 15.0).color(T.on_accent)))
                .child(text("0:00.0 / 0:00.0").live_text(&s.clock).font(NUMBERS).color(T.muted).grow(1.0))
                .child(chip("Play on select".to_owned(), s.autoplay, |s, _| s.autoplay = !s.autoplay));
            let detail = format!("{}, {} Hz, {} ch", clock(seconds), rate, channels);
            (Some(tools), wave, Some(drag_card(id, "Drag sound out", detail)))
        }
        Preview::Failed(reason) => (None, el().w_full().grow(1.0).center().fill(T.well).child(text(format!("Can't preview this file: {reason}")).font(BODY).color(T.muted).wrap().px(20.0)), Some(drag_card(id, "Drag file out", bytes(asset.size)))),
        _ => (None, el().key("loading").w_full().grow(1.0).fill(T.well), None),
    };
    if let Some(t) = tools { body = body.child(t); }
    body = body.child(col().w_full().grow(1.0).min_h(120.0).justify_center().fill(T.well).clip_rounded(WELL_RADIUS).child(stage_body));
    body = body.children(extra);

    let mut facts_block = col().w_full().gap(3.0).child(fact("Size on disk", bytes(asset.size))).child(fact("Format", asset.ext.to_owned()));
    let detail = facts.map(|_| facts_line(asset, facts)).filter(|t| !t.is_empty());
    if let (Some(detail), Kind::Texture) = (detail, asset.kind) { facts_block = facts_block.child(fact("Pixels", detail)); }
    facts_block = facts_block.child(fact("Pack", s.lib.packs[asset.pack as usize].name.clone()));
    body = body.child(facts_block);

    let path = s.asset_path(id).map(|p| p.display().to_string()).unwrap_or_default();
    body = body.child(row().w_full().gap(8.0)
        .child(outlined("Show in Explorer".to_owned(), move |s, _| s.reveal(id)))
        .child(outlined("Copy path".to_owned(), move |_, tx| tx.copy(path.clone()))));
    shell.child(body)
}

pub fn view(s: &App, _: &mut Cx<App>) -> El {
    let main = surface(col().key("main").grow(1.0).h_full().min_w(0.0).min_h(0.0))
        .child(toolbar(s))
        .child(filter_bar(s))
        .child(grid(s))
        .child(status_bar(s));
    row().key("root").w_full().h_full().fill(T.bg).padding(WINDOW_GAP).gap(WINDOW_GAP)
        .child(sidebar(s))
        .child(main)
        .child(inspector(s))
}

const WINDOW_GAP: f32 = 8.0;

pub fn on_key(s: &mut App, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
    use rune::Key;
    if tx.focused().is_some_and(|f| f.contains("search")) {
        if matches!(event.key, Key::Escape) { tx.blur(); return true; }
        return false;
    }
    let cols = s.columns.max(1) as isize;
    match &event.key {
        Key::Left => s.step_selection(-1, tx),
        Key::Right => s.step_selection(1, tx),
        Key::Up => s.step_selection(-cols, tx),
        Key::Down => s.step_selection(cols, tx),
        Key::Text(t) if t == " " => s.toggle_playback(),
        Key::Text(t) if t == "/" => s.search.focused = true,
        Key::Escape if !s.search.text.is_empty() => {
            s.search = rune::TextEdit::new("");
            s.refilter(tx);
        }
        _ => return false,
    }
    true
}
