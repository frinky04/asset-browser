//! The view. Lifted neutral panels on a near-black window, hairlines for structure, white for
//! what's active, and Inter with tabular figures wherever numbers line up. The assets are the
//! only colour on screen.

use std::sync::LazyLock;

use rune::color::hex;
use rune::{Color, Cursor, Cx, El, GrowthSpec, Icon, Phase, PointerButton, ScrollSpec, SurfaceFit, TextStyle, Tx, col, el, icon, memo, row, text};
use rune_kit::{Kit, PaletteSeed, ScrollBar, TextInput};

use crate::app::{App, GRID_GAP, GRID_INSET, GRID_PAD, INSPECTOR_DEFAULT, TILE_PAD, Preview, bytes, clock, commas, ladder};
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
    bg: color("#090909"),
    panel: color("#151515"),
    well: color("#1B1B1B"),
    raised: color("#292929"),
    line: color("#2D2D2D"),
    text: color("#F2F2F2"),
    muted: color("#A6A6A6"),
    faint: color("#858585"),
    accent: color("#F2F2F2"),
    on_accent: color("#0A0A0A"),
    hover: color("#FFFFFF0C"),
});

const PANEL_RADIUS: f32 = 12.0;
const WELL_RADIUS: f32 = 8.0;
const CONTROL_RADIUS: f32 = 7.0;
pub const WINDOW_GAP: f32 = 8.0;
pub const SIDEBAR_W: f32 = 244.0;
/// The outer height a segmented control comes to, shared with the controls beside them.
const CONTROL_H: f32 = 30.0;
const TILE_GAP: f32 = 7.0;
const TILE_RADIUS: f32 = 11.0;
const EDGE_STRONG: &str = "#484848";
/// Below this the inspector gives its details' room to the preview.
const SHORT_WINDOW: f32 = 740.0;

fn kit() -> &'static Kit {
    static K: LazyLock<KitCell> = LazyLock::new(|| {
        let mut k = Kit::from_palette(PaletteSeed { bg: T.panel, fg: T.text, accent: T.accent });
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

const NAME: TextStyle = TextStyle::new(13.0, 500.0).lh(18.0);
const LABEL: TextStyle = TextStyle::new(12.5, 500.0).lh(16.0);
const BODY: TextStyle = TextStyle::new(13.0, 500.0).lh(17.0);
const SMALL: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0).features("'tnum' 1");
const NUMBERS: TextStyle = TextStyle::new(12.0, 500.0).lh(16.0).features("'tnum' 1");
const TITLE: TextStyle = TextStyle::new(16.0, 600.0).lh(21.0);
const FILE_TITLE: TextStyle = TextStyle::new(15.0, 600.0).lh(20.0);

const SEARCH: Icon = Icon { name: "ab-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M10.5 10.5L14 14" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"# };
const IMAGE: Icon = Icon { name: "ab-image", view: 16.0, body: r#"<rect x="2" y="3" width="12" height="10" rx="2" stroke="currentColor" stroke-width="1.3" fill="none"/><circle cx="5.6" cy="6.4" r="1" fill="currentColor"/><path d="M2.6 11.4L6.2 8.2l2.3 2.1 1.9-1.6 3 2.7" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round" stroke-linecap="round"/>"# };
const CUBE: Icon = Icon { name: "ab-cube", view: 16.0, body: r#"<path d="M8 1.8l5.4 3v6.4L8 14.2l-5.4-3V4.8L8 1.8zM2.8 4.9L8 8l5.2-3.1M8 8v6" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round"/>"# };
const SOUND: Icon = Icon { name: "ab-sound", view: 16.0, body: r#"<path d="M2.5 6.5v3M5.25 4v8M8 2v12M10.75 4.5v7M13.5 6.5v3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"# };
const GRID: Icon = Icon { name: "ab-grid", view: 16.0, body: r#"<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="9" y="2.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="2.5" y="9" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="9" y="9" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/>"# };
const PLAY: Icon = Icon { name: "ab-play", view: 16.0, body: r#"<path d="M5 3.2v9.6L13 8 5 3.2z" fill="currentColor"/>"# };
const PAUSE: Icon = Icon { name: "ab-pause", view: 16.0, body: r#"<path d="M4.5 3h2.5v10H4.5zM9 3h2.5v10H9z" fill="currentColor"/>"# };
const FOLDER: Icon = Icon { name: "ab-folder", view: 16.0, body: r#"<path d="M2 4.5A1.5 1.5 0 013.5 3h2.6l1.5 1.7h4.9A1.5 1.5 0 0114 6.2v5.3a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 11.5v-7z" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round"/>"# };
const COPY: Icon = Icon { name: "ab-copy", view: 16.0, body: r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1.5" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M10.5 3.5v-.0A1 1 0 009.5 2.5h-6a1 1 0 00-1 1v6a1 1 0 001 1" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round"/>"# };
const CHECK: Icon = Icon { name: "ab-check", view: 16.0, body: r#"<path d="M3.5 8.5l3 3 6-7" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"# };
const GRIP: Icon = Icon { name: "ab-grip", view: 16.0, body: r#"<g fill="currentColor"><circle cx="5.5" cy="3.5" r="1.35"/><circle cx="10.5" cy="3.5" r="1.35"/><circle cx="5.5" cy="8" r="1.35"/><circle cx="10.5" cy="8" r="1.35"/><circle cx="5.5" cy="12.5" r="1.35"/><circle cx="10.5" cy="12.5" r="1.35"/></g>"# };

fn label(s: impl Into<String>, style: TextStyle, c: Color) -> El {
    text(s.into()).font(style).color(c)
}

/// A lifted surface floating on the near-black window.
fn surface(e: El) -> El {
    e.fill(T.panel).radius(PANEL_RADIUS).border(1.0, T.line)
}

fn hairline() -> El {
    el().w_full().h(1.0).fill(T.line).a11y_hidden()
}

/// A quiet outlined button with an icon.
fn outlined(name: &str, glyph: &'static Icon, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    row().key(name.to_owned()).role("button").focusable(true).label(name.to_owned()).px(11.0).h(30.0).gap(7.0).radius(CONTROL_RADIUS).items_center()
        .border(1.0, T.line).hover(T.hover).cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(icon(glyph, 14.0).color(T.muted))
        .child(label(name.to_owned(), LABEL, T.text))
}

struct Segment<V> {
    name: String,
    count: Option<String>,
    glyph: Option<&'static Icon>,
    value: V,
}

fn plain<V>(name: impl Into<String>, value: V) -> Segment<V> {
    Segment { name: name.into(), count: None, glyph: None, value }
}

/// A row of mutually exclusive choices inside a hairline. The current one is raised.
fn segments<V: Copy + PartialEq + 'static>(key: &str, items: Vec<Segment<V>>, current: V, on: impl Fn(&mut App, V, &mut Tx) + Clone + 'static) -> El {
    let mut bar = row().key(key.to_owned()).h(CONTROL_H).radius(CONTROL_RADIUS + 2.0).border(1.0, T.line).padding(2.0).gap(1.0).items_center();
    for Segment { name, count, glyph, value } in items {
        let selected = value == current;
        let on = on.clone();
        let whole = count.as_ref().map_or_else(|| name.clone(), |c| format!("{name} {c}"));
        let mut item = row().role("radio").checked(selected).label(whole.clone()).focusable(true).key(whole).px(9.0).h(26.0).gap(6.0).radius(CONTROL_RADIUS).items_center()
            .fill(if selected { T.raised } else { Color::TRANSPARENT })
            .hover(if selected { Color::TRANSPARENT } else { T.hover })
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| on(s, value, tx));
        if let Some(g) = glyph { item = item.child(icon(g, 14.0).color(if selected { T.text } else { T.muted })); }
        item = item.child(label(name, LABEL, if selected { T.text } else { T.muted }));
        if let Some(c) = count { item = item.child(label(c, SMALL, if selected { T.muted } else { T.faint })); }
        bar = bar.child(item);
    }
    bar
}

/// A count that stays narrow enough to sit in a tab.
fn short_count(n: u32) -> String {
    if n < 10_000 { commas(n as usize) } else { format!("{:.1}k", f64::from(n) / 1000.0) }
}

/// Roughly how wide the kind tabs draw, to know what fits beside them.
fn kind_tabs_width(s: &App) -> f32 {
    let r = &s.results;
    let tab = |name: &str, n: u32| {
        let count = if s.is_compact() { 0.0 } else { 6.0 + short_count(n).len() as f32 * 7.0 };
        18.0 + 20.0 + name.len() as f32 * 7.2 + count
    };
    6.0 + tab("All", r.any_kind) + tab("Textures", r.by_kind[0]) + tab("Models", r.by_kind[1]) + tab("Audio", r.by_kind[2])
}

fn kind_tabs(s: &App) -> El {
    let r = &s.results;
    let compact = s.is_compact();
    let tab = |name: &str, n: u32, glyph: &'static Icon, value| Segment { name: name.to_owned(), count: (!compact).then(|| short_count(n)), glyph: Some(glyph), value };
    let items = vec![
        tab("All", r.any_kind, &GRID, None),
        tab("Textures", r.by_kind[0], &IMAGE, Some(Kind::Texture)),
        tab("Models", r.by_kind[1], &CUBE, Some(Kind::Model)),
        tab("Audio", r.by_kind[2], &SOUND, Some(Kind::Audio)),
    ];
    segments("kinds", items, s.filter.kind, |s, v, tx| s.set_kind(v, tx))
}

fn search_box(s: &App) -> El {
    let mut field = TextInput::new(&s.search)
        .placeholder("Search assets")
        .label("Search assets")
        .plain()
        .width(None)
        .on_edit(|s: &mut App| &mut s.search)
        .on_change(|s: &mut App, tx| s.refilter(tx));
    let empty = s.search.text.is_empty();
    if !empty { field = field.clear_button(); }
    let mut bar = row().key("searchbar").w(if s.is_compact() { 190.0 } else { 280.0 }).h(34.0).pl(11.0).pr(6.0).gap(4.0).radius(CONTROL_RADIUS + 2.0).items_center().border(1.0, T.line).cursor(Cursor::Text)
        .child(icon(&SEARCH, 15.0).color(T.muted))
        .child(el().grow(1.0).min_w(0.0).child(field.build(kit()).key("search")));
    if empty {
        bar = bar.child(el().px(7.0).h(22.0).radius(6.0).center_y().fill(T.raised).a11y_hidden().child(label("/", SMALL, T.muted)));
    }
    bar
}

fn header(s: &App) -> El {
    let scope = s.filter.pack.and_then(|p| s.lib.packs.get(p as usize)).map_or("All packs", |p| p.name.as_str());
    let sorts = vec![plain("Pack", Sort::Pack), plain("Name", Sort::Name), plain("Largest", Sort::Largest), plain("Newest", Sort::Newest)];
    let mut title = row().grow(1.0).min_w(0.0).gap(10.0).items_end()
        .child(el().min_w(0.0).child(text(scope.to_owned()).key("scope").font(TITLE).color(T.text).truncate()));
    if !s.is_compact() {
        title = title.child(label(format!("{} assets", commas(s.results.ids.len())), SMALL, T.muted).mb(2.0));
    }
    row().key("toolbar").w_full().h(58.0).px(GRID_PAD).gap(12.0).items_center()
        .child(title)
        .child(if s.grid_width < 700.0 { sort_cycle(s) } else { segments("sort", sorts, s.filter.sort, |s, v, tx| s.set_sort(v, tx)) })
        .child(search_box(s))
}

/// The sort control when there's no room for all four choices.
fn sort_cycle(s: &App) -> El {
    let (name, next) = match s.filter.sort {
        Sort::Pack => ("Pack", Sort::Name),
        Sort::Name => ("Name", Sort::Largest),
        Sort::Largest => ("Largest", Sort::Newest),
        Sort::Newest => ("Newest", Sort::Pack),
    };
    let title = format!("Sort: {name}");
    row().key("sort-cycle").role("button").focusable(true).label(title.clone()).px(10.0).h(30.0).radius(CONTROL_RADIUS).items_center().border(1.0, T.line).hover(T.hover).cursor(Cursor::Pointer)
        .on_press_tx(move |s: &mut App, tx| s.set_sort(next, tx))
        .child(label(title, LABEL, T.muted))
}

fn chip(key: String, name: String, count: Option<String>, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    let mut c = row().key(key.clone()).role("button").focusable(true).label(key).px(9.0).h(26.0).gap(5.0).radius(CONTROL_RADIUS).items_center()
        .fill(if on { T.raised } else { Color::TRANSPARENT })
        .hover(if on { Color::TRANSPARENT } else { T.hover })
        .cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(label(name, SMALL, if on { T.text } else { T.muted }));
    if let Some(n) = count { c = c.child(label(n, SMALL, if on { T.muted } else { T.faint })); }
    c
}

/// An on/off control, drawn so its state reads without comparing it to its neighbours.
fn toggle(name: &str, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    let mut t = row().key(name.to_owned()).role("switch").checked(on).focusable(true).label(name.to_owned()).px(10.0).h(CONTROL_H).gap(5.0).radius(CONTROL_RADIUS).items_center()
        .border(1.0, T.line)
        .fill(if on { T.raised } else { Color::TRANSPARENT })
        .hover(if on { Color::TRANSPARENT } else { T.hover })
        .cursor(Cursor::Pointer)
        .on_press_tx(f);
    if on { t = t.child(icon(&CHECK, 12.0).color(T.text)); }
    t.child(label(name.to_owned(), LABEL, if on { T.text } else { T.muted }))
}

/// A chip that's its own label.
fn tool(name: &str, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    chip(name.to_owned(), name.to_owned(), None, on, f)
}

fn filter_bar(s: &App) -> El {
    // Whole chips only: the rarest formats go behind "+N" when they don't all fit.
    let chip_width = |e: &str| 18.0 + e.len() as f32 * 7.0 + 2.0;
    const MORE_WIDTH: f32 = 42.0;
    let mut room = s.grid_width - GRID_PAD * 2.0 - 12.0 * 2.0 - kind_tabs_width(s) - 112.0 - 8.0;
    let mut all: Vec<(&'static str, u32)> = s.results.by_ext.clone();
    let total: f32 = all.iter().map(|(e, _)| chip_width(e)).sum();
    if total > room { room -= MORE_WIDTH; }
    let mut fit = 0;
    let mut used = 0.0;
    while fit < all.len() && used + chip_width(all[fit].0) <= room {
        used += chip_width(all[fit].0);
        fit += 1;
    }
    if fit > 0 && let Some(picked) = s.filter.ext.and_then(|e| all.iter().position(|(x, _)| *x == e)).filter(|p| *p >= fit) {
        all.swap(fit - 1, picked);
    }
    let mut formats = row().gap(2.0).items_center();
    for (e, n) in &all[..fit] {
        let e: &'static str = e;
        let name = format!("{e} {}", commas(*n as usize));
        formats = formats.child(chip(name.clone(), e.to_owned(), None, s.filter.ext == Some(e), move |s, tx| s.set_ext(Some(e), tx)).help(name));
    }
    let hidden: Vec<(&'static str, u32)> = all[fit..].to_vec();
    if !hidden.is_empty() {
        let title = format!("+{}", hidden.len());
        formats = formats.child(chip("more-formats".to_owned(), title, None, false, move |_, tx| {
            let mut menu = rune::PopupMenu::new();
            for (e, n) in &hidden { menu = menu.item(rune::PopupItem::new(*e, format!("{e}  {}", commas(*n as usize)))); }
            tx.popup_menu(menu, "more-formats", |s: &mut App, picked, tx| {
                let found = picked.and_then(|p| s.results.by_ext.iter().map(|(e, _)| *e).find(|e| *e == p));
                if let Some(e) = found { s.set_ext(Some(e), tx); }
            });
        }).help("More formats"));
    }
    let mut filters = row().key("filters").w_full().pb(12.0).px(GRID_PAD).gap(12.0).items_center().child(kind_tabs(s));
    if !all.is_empty() { filters = filters.child(formats.radius(CONTROL_RADIUS + 2.0).border(1.0, T.line).padding(2.0)); }
    filters.child(el().grow(1.0))
        .child(toggle("Every format", s.filter.all_formats, |s, tx| s.toggle_all_formats(tx)).help("Show every copy of a model when a pack ships it as GLB, OBJ and DAE"))
}

fn sidebar(s: &App) -> El {
    let mut list = col().key("packlist").w_full().px(8.0).pb(8.0).gap(1.0);
    let row_for = |name: String, count: usize, on: bool, pack: Option<u32>| {
        row().key(name.clone()).role("button").focusable(true).label(name.clone()).w_full().h(32.0).px(10.0).radius(CONTROL_RADIUS).items_center().gap(8.0)
            .fill(if on { T.raised } else { Color::TRANSPARENT })
            .hover(if on { Color::TRANSPARENT } else { T.hover })
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.set_pack(pack, tx))
            .child(el().grow(1.0).min_w(0.0).child(text(name).font(BODY).color(if on { T.text } else { T.muted }).truncate()))
            .child(label(commas(count), SMALL, if on { T.muted } else { T.faint }))
    };
    list = list.child(row_for("All packs".to_owned(), s.results.any_pack as usize, s.filter.pack.is_none(), None));
    for (i, pack) in s.lib.packs.iter().enumerate() {
        let i = i as u32;
        let count = s.results.by_pack.get(i as usize).copied().unwrap_or(0) as usize;
        // A filter's dead ends only add noise; the selected pack stays so it can be cleared.
        if count == 0 && s.filter.pack != Some(i) { continue }
        list = list.child(row_for(pack.name.clone(), count, s.filter.pack == Some(i), Some(i)));
    }
    let scroller = col().key("packs").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list).scroll_bar(kit());
    surface(col().key("sidebar").w(SIDEBAR_W).h_full().min_h(0.0))
        .child(col().w_full().h(58.0).px(18.0).justify_center()
            .child(text("Library").key("library-title").font(TITLE).color(T.text))
            .child(label(format!("{} files in {} packs", commas(s.lib.assets.len()), s.lib.packs.len()), SMALL, T.muted)))
        .child(el().w_full().grow(1.0).min_h(0.0).child(scroller))
        .child(hairline())
        .child(el().w_full().padding(8.0).child(
            row().key("change-root").role("button").focusable(true).label("Change library folder").w_full().h(32.0).px(10.0).gap(8.0).radius(CONTROL_RADIUS).items_center()
                .hover(T.hover).cursor(Cursor::Pointer)
                .on_press_tx(|_: &mut App, tx: &mut Tx| {
                    tx.open_folder(rune::FileDialogOptions { title: "Choose a library folder".into(), ..Default::default() }, |s: &mut App, picked, _| {
                        if let Ok(Some(path)) = picked { s.change_root(path); }
                    });
                })
                .child(icon(&FOLDER, 14.0).color(T.muted))
                .child(label("Change library folder", BODY, T.muted))))
}

fn facts_line(asset: &Asset, facts: Option<Facts>) -> String {
    match (asset.kind, facts) {
        (_, Some(f)) if f.failed => "Can't preview".to_owned(),
        (Kind::Texture, Some(f)) => format!("{}×{}", f.width, f.height),
        (Kind::Model, Some(f)) => format!("{} tris", commas(f.width as usize)),
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
    let inner = width - TILE_PAD * 2.0;
    let mut picture = el().w(inner).h(s.picture_height()).radius(WELL_RADIUS).fill(T.well).center();
    picture = if asset.kind == Kind::Audio {
        // A waveform is drawn to its edges, so it's the one thumbnail that needs a margin.
        picture.child(el().w(inner - 24.0).h((s.picture_height() - 14.0).max(1.0)).pixels_fit(&px, SurfaceFit::Fit))
    } else {
        picture.pixels_fit(&px, SurfaceFit::Fit)
    };
    if failed { picture = picture.child(icon(kind_icon(asset.kind), 22.0).color(T.faint)); }
    // Lifts the well under the pointer too, without the view running for every tile crossed.
    picture = picture.child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).radius(WELL_RADIUS).fill(color("#FFFFFF10")).border(1.0, color("#3A3A3A")).inert().group_opacity(0.0, 1.0));
    let max = s.drag_max;
    col().key(format!("a{id}")).role("button").label(asset.name().to_owned()).w(width).padding(TILE_PAD).gap(TILE_GAP).radius(TILE_RADIUS).cursor(Cursor::Grab).hover_group()
        .fill(if selected { T.raised } else { Color::TRANSPARENT })
        .border(1.0, if selected { T.accent } else { Color::TRANSPARENT })
        .hover(if selected { Color::TRANSPARENT } else { T.hover })
        .press_scale(0.985)
        .opacity(if s.dragging == Some(id) { 0.45 } else { 1.0 })
        .on_press_tx(move |s: &mut App, tx| s.select(id, tx))
        .on_drag(move |s: &mut App, drag, tx| {
            let max = if s.lib.assets[id as usize].kind == Kind::Texture { max } else { None };
            s.drag_gesture(id, max, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(picture)
        .child(col().w_full().px(2.0)
            // The extension has its own column, and what tells siblings apart is usually a
            // number at the end, so a long name gives way in the middle.
            .child(text(asset.stem().to_owned()).font(NAME).color(T.text).truncate_middle().w_full())
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
        let mut line = row().w_full().h(s.row_height()).px(GRID_INSET).gap(GRID_GAP).items_start();
        for (i, id) in ids[from..to].iter().enumerate() {
            line = line.child(tile(s, *id, width, selected == Some(i)));
        }
        line
    })
}

impl App {
    /// Which of Fit (0) or 1:1 (1) the texture preview is at, or neither (2).
    fn zoom_mode(&self) -> u8 {
        let Preview::Texture(image) = &self.preview else { return 2 };
        let view = &self.stage.view;
        if view.tile { 2 }
        else if view.zoom == 1.0 { 0 }
        else if (view.zoom - actual_size_zoom(self.stage_size, (image.width, image.height), self.scale)).abs() < 1e-3 { 1 }
        else { 2 }
    }

    /// A narrow grid drops the counts and captions its controls can live without.
    fn is_compact(&self) -> bool {
        self.grid_width < 780.0
    }

    /// Waveforms are wide and thin, so an audio-only view doesn't spend a square on each.
    fn picture_height(&self) -> f32 {
        let w = self.tile_width() - TILE_PAD * 2.0;
        if self.filter.kind == Some(Kind::Audio) { (w * 0.46).floor() } else { w }
    }

    pub fn row_height(&self) -> f32 {
        // The tile's padding, its picture, gap and two lines of text, then the gutter.
        self.picture_height() + TILE_PAD * 2.0 + TILE_GAP + 18.0 + 16.0 + 6.0
    }
}

fn grid(s: &App) -> El {
    let count = s.row_count();
    let rows = col().key("rows").w_full().growth(GrowthSpec::default())
        .virtual_rows(count, s.row_height(), |_: &App, i| format!("r{i}").into(), grid_row)
        .virtual_rows_version((s.generation, s.columns))
        .on_visible_rows(|s: &mut App, range| { s.request_range(range); false });
    let empty = s.results.ids.is_empty() && !s.scanning;
    let body = col().key("grid").w_full().grow(1.0).min_h(0.0).pt(GRID_INSET)
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
    let sizes = vec![plain("S", 120.0f32), plain("M", 158.0), plain("L", 232.0)];
    let drag_sizes = vec![plain("Full size", None), plain("1024", Some(1024u32)), plain("512", Some(512)), plain("256", Some(256))];
    let (made, total) = s.thumbs.progress();
    let working = s.scanning || made < total;
    let mut bar = row().key("status").w_full().h(46.0).px(GRID_PAD).gap(12.0).items_center();
    bar = if working || !s.is_compact() {
        bar.child(text("Making thumbnails: 00,000 of 00,000").live_text(&s.status).font(SMALL).color(T.muted).truncate_fade().grow(1.0).min_w(0.0))
    } else {
        bar.child(el().grow(1.0))
    };
    if let Some(m) = &s.message { bar = bar.child(el().min_w(0.0).child(text(m.clone()).key("message").font(SMALL).color(T.text).truncate())); }
    if s.filter.kind.is_none_or(|k| k == Kind::Texture) {
        let mut drag_at = row().gap(8.0).items_center();
        if !s.is_compact() { drag_at = drag_at.child(label("Drag textures at", SMALL, T.muted)); }
        bar = bar.child(drag_at.child(segments("dragmax", drag_sizes, s.drag_max, |s, v, _| s.drag_max = v)));
    }
    bar.child(segments("tilesize", sizes, s.tile_size, |s, v, _| s.set_tile_size(v)))
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
            let before = s.zoom_mode();
            s.stage.view.offset = zoom_about(s.stage.view.offset, at, next / s.stage.view.zoom);
            s.stage.view.zoom = next;
            // The Fit / 1:1 control follows the zoom, so leaving either needs one view run.
            if s.zoom_mode() == before { tx.live_only(); }
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

/// A rung of the export ladder: a square sized by the export, ready to be dragged out.
fn rung(id: u32, name: String, max: Option<u32>, side: f32, on: bool) -> El {
    col().key(format!("rung-{name}")).role("button").label(format!("Drag out at {name}")).items_center().justify_end().gap(6.0).h(84.0).cursor(Cursor::Grab)
        .help(format!("Drag out at {name}"))
        .press_scale(0.96)
        .on_drag(move |s: &mut App, drag, tx| {
            s.drag_gesture(id, max, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(el().w(side).h(side).radius(6.0).center().fill(T.raised).hover(T.hover).border(1.0, if on { T.accent } else { color(EDGE_STRONG) })
            .child(icon(&GRIP, 12.0).color(if on { T.text } else { T.muted })))
        .child(label(name, SMALL, if on { T.text } else { T.muted }))
}

fn texture_ladder(s: &App, id: u32, width: u32, height: u32) -> Option<El> {
    let longest = width.max(height);
    let sizes = ladder(longest);
    if sizes.is_empty() { return None }
    let side = |n: u32| 24.0 + 34.0 * (n as f32 / longest as f32).sqrt();
    let mut steps = row().key("ladder").w_full().gap(8.0).items_end().justify_between()
        .child(rung(id, format!("{width}×{height}"), None, side(longest), s.drag_max.is_none()));
    for n in sizes { steps = steps.child(rung(id, n.to_string(), Some(n), side(n), s.drag_max == Some(n))); }
    let filters = vec![plain("Smooth", false), plain("Hard pixels", true)];
    Some(col().w_full().gap(8.0)
        .child(label("Or drag out a specific size", SMALL, T.muted))
        .child(steps)
        .child(row().w_full().justify_between().items_center()
            .child(label("Resize filter", SMALL, T.muted))
            .child(segments("filter", filters, s.nearest, |s, v, _| s.nearest = v))))
}

/// The panel's main action: a white bar you grab.
fn drag_bar(id: u32, max: Option<u32>, title: &str, detail: String) -> El {
    row().key("drag-card").role("button").label(title.to_owned()).w_full().h(40.0).px(13.0).gap(8.0).radius(CONTROL_RADIUS + 2.0).items_center().fill(T.accent).cursor(Cursor::Grab)
        .press_scale(0.985)
        .on_drag(move |s: &mut App, drag, tx| {
            s.drag_gesture(id, max, drag);
            if drag.phase == Phase::Move { tx.live_only(); }
        })
        .child(icon(&GRIP, 14.0).color(T.on_accent))
        .child(label(title.to_owned(), LABEL, T.on_accent))
        .child(el().grow(1.0))
        .child(el().min_w(0.0).child(text(detail).font(SMALL).color(T.on_accent.alpha(0.6)).truncate()))
}

/// Label-value rows in a hairline card, divided as one list.
fn facts_card(rows: Vec<(&str, String)>) -> El {
    let mut card = col().key("facts").w_full().radius(WELL_RADIUS + 1.0).border(1.0, T.line);
    for (i, (name, value)) in rows.into_iter().enumerate() {
        if i > 0 { card = card.child(hairline()); }
        card = card.child(row().w_full().h(30.0).px(12.0).gap(12.0).items_center().justify_between()
            .child(label(name.to_owned(), SMALL, T.muted))
            .child(el().min_w(0.0).child(text(value).font(NUMBERS).color(T.text).truncate())));
    }
    card
}

/// What the inspector shows under its title for the selected asset's preview.
struct Pane {
    tools: Option<El>,
    stage: El,
    hint: Option<&'static str>,
    /// A control floated over the stage's corner.
    corner: Option<El>,
    /// Controls docked along the stage's bottom edge.
    dock: Option<El>,
    actions: Vec<El>,
}

fn stage_el() -> El {
    el().key(STAGE_KEY.to_owned()).w_full().grow(1.0).min_h(120.0).effect(STAGE_MARK)
}

fn pane(s: &App, id: u32, asset: &Asset) -> Pane {
    match &s.preview {
        Preview::Texture(image) => {
            let view = s.stage.view;
            let size = (image.width, image.height);
            let zoom = vec![plain("Fit", 0u8), plain("1:1", 1u8)];
            let tools = row().key("tools").w_full().flex_wrap().gap(6.0).items_center()
                .child(segments("zoom", zoom, s.zoom_mode(), move |s, mode, _| {
                    s.set_tile(false);
                    s.reset_view();
                    if mode == 1 { s.stage.view.zoom = actual_size_zoom(s.stage_size, size, s.scale); }
                }))
                .child(toggle("Tile", view.tile, move |s, _| { let on = !s.stage.view.tile; s.set_tile(on); }))
                .child(toggle("Seams", view.seams && view.tile, |s, _| {
                    s.stage.view.seams = !s.stage.view.seams;
                    if s.stage.view.seams && !s.stage.view.tile { s.set_tile(true); }
                }))
                .child(el().grow(1.0))
                .child(segments("sampling", vec![plain("Pixels", false), plain("Smooth", true)], view.smooth, |s, v, _| s.stage.view.smooth = v));
            let detail = match s.drag_max.filter(|m| *m < image.width.max(image.height)) {
                Some(max) => format!("at {max}"),
                None => format!("{}×{}", image.width, image.height),
            };
            let mut actions = vec![drag_bar(id, s.drag_max, "Drag texture out", detail)];
            actions.extend(texture_ladder(s, id, image.width, image.height));
            Pane { tools: Some(tools), stage: texture_stage_handlers(stage_el()), hint: None, corner: None, dock: None, actions }
        }
        Preview::Model(m) => {
            let images = match m.images.len() {
                0 => "no textures".to_owned(),
                1 => "1 texture".to_owned(),
                n => format!("{n} textures"),
            };
            let mut actions = vec![drag_bar(id, None, "Drag model out", format!("{} tris, {images}", commas(m.triangles())))];
            if matches!(asset.ext, "obj" | "dae") {
                actions.push(row().w_full().justify_between().items_center()
                    .child(label("Material and texture files", SMALL, T.muted))
                    .child(segments("companions", vec![plain("Include", true), plain("Leave out", false)], s.companions, |s, v, _| s.companions = v)));
            }
            Pane { tools: None, stage: model_stage_handlers(stage_el()), hint: Some("Drag to orbit, right-drag to pan, scroll to zoom"), corner: Some(tool("Reset view", false, |s, _| s.reset_view())), dock: None, actions }
        }
        Preview::Audio { seconds, rate, channels } => {
            let playing = s.audio.is_playing();
            let wave = el().key("waveform").w_full().h(200.0).cursor(Cursor::Pointer)
                .pixels(&s.wave)
                .on_layout(|s: &mut App, r: [f32; 4]| s.stage_size = [r[2], r[3]])
                .on_drag(|s: &mut App, drag, tx| {
                    if drag.phase != Phase::Up { s.seek_fraction(drag.x / drag.w.max(1.0), tx); }
                })
                .child(el().key("playhead").w(1.5).h_full().fill(T.accent).absolute(0.0, 0.0).live_offset(&s.playhead));
            let dock = row().key("tools").w_full().gap(10.0).items_center()
                .child(el().key("play").role("button").label(if playing { "Pause" } else { "Play" }).focusable(true).size(30.0, 30.0).radius(15.0).center().fill(T.accent).cursor(Cursor::Pointer)
                    .on_press(|s: &mut App| s.toggle_playback())
                    .child(icon(if playing { &PAUSE } else { &PLAY }, 14.0).color(T.on_accent)))
                .child(text("0:00.0 / 0:00.0").live_text(&s.clock).font(NUMBERS).color(T.muted).grow(1.0))
                .child(toggle("Play on select", s.autoplay, |s, _| s.autoplay = !s.autoplay));
            let detail = format!("{}, {} Hz, {} ch", clock(*seconds), commas(*rate as usize), channels);
            Pane { tools: None, stage: col().w_full().px(16.0).pb(36.0).child(wave), hint: None, corner: None, dock: Some(dock), actions: vec![drag_bar(id, None, "Drag sound out", detail)] }
        }
        Preview::Failed(reason) => Pane {
            tools: None,
            stage: el().w_full().grow(1.0).center().child(text(format!("Can't preview this file: {reason}")).font(BODY).color(T.muted).wrap().px(20.0)),
            hint: None,
            corner: None,
            dock: None,
            actions: vec![drag_bar(id, None, "Drag file out", bytes(asset.size))],
        },
        _ => Pane { tools: None, stage: el().key("loading").w_full().grow(1.0), hint: None, corner: None, dock: None, actions: Vec::new() },
    }
}

fn inspector(s: &App) -> El {
    let shell = surface(col().key("inspector").w(s.inspector_width()).h_full().min_h(0.0));
    let Some((id, asset)) = s.selected.and_then(|i| s.lib.assets.get(i as usize).map(|a| (i, a))) else {
        return shell.center()
            .child(icon(&GRID, 24.0).color(T.faint))
            .child(text("Select an asset").font(TextStyle::new(14.0, 600.0).lh(19.0)).color(T.text).mt(12.0))
            .child(label("Preview textures, models and audio here.", SMALL, T.muted).mt(4.0));
    };
    let Pane { tools, stage, hint, corner, dock, actions } = pane(s, id, asset);
    let mut body = col().w_full().padding(14.0).gap(12.0).min_h(0.0).grow(1.0);
    body = body.child(col().w_full().gap(2.0)
        .child(text(asset.name().to_owned()).key("name").font(FILE_TITLE).color(T.text).wrap_anywhere().max_lines(2))
        .child(text(asset.folder().to_owned()).font(SMALL).color(T.muted).truncate_head().w_full()));
    if let Some(t) = tools { body = body.child(t); }
    let mut well = col().w_full().grow(1.0).min_h(120.0).justify_center().radius(WELL_RADIUS).fill(T.well).child(stage);
    if let Some(h) = hint {
        well = well.child(el().pin(Some(12.0), None, None, Some(10.0)).inert().child(label(h, SMALL, T.faint)));
    }
    if let Some(c) = corner {
        well = well.child(el().pin(None, Some(8.0), Some(8.0), None).child(c));
    }
    if let Some(d) = dock {
        well = well.child(el().pin(Some(12.0), None, Some(12.0), Some(10.0)).child(d));
    }
    body = body.child(well).children(actions);

    let mut facts = vec![("Size on disk", bytes(asset.size)), ("Format", asset.ext.to_owned())];
    if asset.kind == Kind::Texture {
        let detail = facts_line(asset, s.thumbs.facts(asset.key));
        if !detail.is_empty() { facts.push(("Pixels", detail)); }
    }
    facts.push(("Pack", s.lib.packs[asset.pack as usize].name.clone()));
    if s.window_height >= SHORT_WINDOW {
        body = body.child(facts_card(facts));
    } else {
        // Short windows keep the facts on one line, so the preview keeps its height.
        let line = facts.into_iter().map(|(_, value)| value).collect::<Vec<_>>().join(", ");
        body = body.child(text(line).key("facts-line").font(SMALL).color(T.muted).truncate().w_full());
    }

    let path = s.asset_path(id).map(|p| p.display().to_string()).unwrap_or_default();
    body = body.child(row().w_full().gap(8.0)
        .child(outlined("Show in Explorer", &FOLDER, move |s, _| s.reveal(id)))
        .child(outlined("Copy path", &COPY, move |_, tx| tx.copy(path.clone()))));
    shell.child(body)
}

/// The gap left of the inspector, which drags to resize it.
fn inspector_handle() -> El {
    el().key("inspector-handle").role("separator").label("Resize preview panel").w(WINDOW_GAP).h_full().center().cursor(Cursor::ResizeColumn).hover_group()
        .on_drag(|s: &mut App, drag, _| match drag.phase {
            Phase::Down => s.drag_last = [s.inspector_width(), 0.0],
            Phase::Move => s.resize_inspector(s.drag_last[0] - drag.dx),
            Phase::Up => s.save_inspector_width(),
        })
        .on_double_press(|s: &mut App| {
            s.resize_inspector(INSPECTOR_DEFAULT);
            s.save_inspector_width();
        })
        .child(el().w(2.0).h(44.0).radius(1.0).fill(T.muted).group_opacity(0.0, 1.0))
}

pub fn view(s: &App, _: &mut Cx<App>) -> El {
    let main = surface(col().key("main").grow(1.0).h_full().min_w(0.0).min_h(0.0))
        .child(header(s))
        .child(filter_bar(s))
        .child(hairline())
        .child(grid(s))
        .child(hairline())
        .child(status_bar(s));
    row().key("root").w_full().h_full().fill(T.bg).padding(WINDOW_GAP)
        .on_layout(|s: &mut App, r: [f32; 4]| { s.window_width = r[2]; s.window_height = r[3]; })
        .child(sidebar(s))
        .child(el().w(WINDOW_GAP).h_full())
        .child(main)
        .child(inspector_handle())
        .child(inspector(s))
}

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
