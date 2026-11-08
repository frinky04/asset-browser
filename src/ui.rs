//! The view. Lifted neutral panels on a near-black window, hairlines for structure, white for
//! what's active, and Inter with tabular figures wherever numbers line up. The assets are the
//! only colour on screen.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rune::color::hex;
use rune::tok::motion;
use rune::{Color, Cursor, Cx, El, GrowthSpec, Icon, Phase, PointerButton, ScrollSpec, Spring, SurfaceFit, TextStyle, Transition, Tx, col, el, icon, memo, row, text};
use rune_kit::{Kit, PaletteSeed, ScrollBar, ScrollBarStyle, TextInput};

use crate::app::{App, GRID_GAP, GRID_INSET, GRID_PAD, INSPECTOR_DEFAULT, Motion, TILE_PAD, Preview, Theme, bytes, clock, commas, ladder};
use crate::library::{Asset, Kind, Sort};
use crate::settings;
use crate::stage::{FOV_DEGREES, STAGE_KEY, STAGE_MARK, actual_size_zoom, zoom_about};
use crate::thumbs::Facts;

pub struct Tokens {
    bg: Color,
    panel: Color,
    well: Color,
    raised: Color,
    pub(crate) line: Color,
    pub(crate) text: Color,
    pub(crate) muted: Color,
    faint: Color,
    pub(crate) accent: Color,
    on_accent: Color,
    pub(crate) hover: Color,
    pub(crate) press: Color,
    well_hover: Color,
    well_edge: Color,
    edge_strong: Color,
    scroll_thumb: Color,
    scroll_thumb_hover: Color,
}

fn color(s: &str) -> Color {
    hex(s).unwrap_or(Color::WHITE)
}

static DARK: LazyLock<Tokens> = LazyLock::new(|| Tokens {
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
    press: color("#FFFFFF18"),
    well_hover: color("#FFFFFF10"),
    well_edge: color("#3A3A3A"),
    edge_strong: color("#484848"),
    scroll_thumb: color("#3C3C3C"),
    scroll_thumb_hover: color("#6A6A6A"),
});

static LIGHT: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    bg: color("#E4E4E6"),
    panel: color("#F8F8F9"),
    well: color("#EDEDEF"),
    raised: color("#DEDEE1"),
    line: color("#D6D6D9"),
    text: color("#161618"),
    muted: color("#5C5C63"),
    faint: color("#7C7C84"),
    accent: color("#161618"),
    on_accent: color("#F8F8F9"),
    hover: color("#0000000A"),
    press: color("#00000016"),
    well_hover: color("#00000010"),
    well_edge: color("#C4C4C8"),
    edge_strong: color("#B4B4B9"),
    scroll_thumb: color("#C8C8CC"),
    scroll_thumb_hover: color("#9A9AA0"),
});

static IS_DARK: AtomicBool = AtomicBool::new(true);

/// Which palette the view builds with. Set at the top of each view run.
pub fn set_dark(dark: bool) {
    IS_DARK.store(dark, Ordering::Relaxed);
}

fn is_dark() -> bool {
    IS_DARK.load(Ordering::Relaxed)
}

/// The current palette, read as `T.text`.
pub struct CurrentPalette;

impl std::ops::Deref for CurrentPalette {
    type Target = Tokens;
    fn deref(&self) -> &Tokens {
        if is_dark() { &DARK } else { &LIGHT }
    }
}

pub static T: CurrentPalette = CurrentPalette;

pub(crate) const PANEL_RADIUS: f32 = 12.0;
pub(crate) const WELL_RADIUS: f32 = 8.0;
const CONTROL_RADIUS: f32 = 7.0;
pub const WINDOW_GAP: f32 = 8.0;
pub const TITLEBAR_H: f32 = 46.0;
pub const SIDEBAR_W: f32 = 244.0;
/// The outer height a segmented control comes to, shared with the controls beside them.
const CONTROL_H: f32 = 30.0;
const SEARCH_W: f32 = 320.0;
/// The inset of a group's contents, the same on every side. Rune draws borders inside the box.
const GROUP_PAD: f32 = 3.0;
/// What's left for a segment inside a group.
const SEGMENT_H: f32 = CONTROL_H - GROUP_PAD * 2.0;
const GROUP_RADIUS: f32 = 9.0;
/// Concentric with the group around it.
const SEGMENT_RADIUS: f32 = GROUP_RADIUS - GROUP_PAD;
const COUNT_SLOT: f32 = 36.0;
const TILE_GAP: f32 = 7.0;
const TILE_RADIUS: f32 = 11.0;
const PACK_ROW: f32 = 32.0;
const PACK_GAP: f32 = 1.0;
const PACK_LEFT: f32 = 8.0;
const PACK_RIGHT: f32 = 12.0;
/// How long a thumbnail takes to appear once it has arrived.
const THUMB_FADE_SECONDS: f32 = 0.16;
/// Below this the inspector gives its details' room to the preview.
const SHORT_WINDOW: f32 = 740.0;

fn kit_for(t: &Tokens) -> KitCell {
    let mut k = Kit::from_palette(PaletteSeed { bg: t.panel, fg: t.text, accent: t.accent });
    k.c.border_focus = t.faint;
    k.c.border_muted = t.scroll_thumb;
    k.c.border_strong = t.scroll_thumb_hover;
    k.radius.md = CONTROL_RADIUS;
    KitCell(k)
}

fn kit() -> &'static Kit {
    static DARK_KIT: LazyLock<KitCell> = LazyLock::new(|| kit_for(&DARK));
    static LIGHT_KIT: LazyLock<KitCell> = LazyLock::new(|| kit_for(&LIGHT));
    if is_dark() { &DARK_KIT.0 } else { &LIGHT_KIT.0 }
}

struct KitCell(Kit);
// SAFETY: built once from plain values and only ever read, on the UI thread.
unsafe impl Send for KitCell {}
// SAFETY: as above.
unsafe impl Sync for KitCell {}

const NAME: TextStyle = TextStyle::new(13.0, 500.0).lh(18.0);
const LABEL: TextStyle = TextStyle::new(12.5, 500.0).lh(16.0);
pub(crate) const BODY: TextStyle = TextStyle::new(13.0, 500.0).lh(17.0);
pub(crate) const SMALL: TextStyle = TextStyle::new(12.0, 400.0).lh(16.0).features("'tnum' 1");
pub(crate) const NUMBERS: TextStyle = TextStyle::new(12.0, 500.0).lh(16.0).features("'tnum' 1");
pub(crate) const TITLE: TextStyle = TextStyle::new(16.0, 600.0).lh(21.0);
const FILE_TITLE: TextStyle = TextStyle::new(15.0, 600.0).lh(20.0);

const SEARCH: Icon = Icon { name: "ab-search", view: 16.0, body: r#"<circle cx="7" cy="7" r="4.5" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M10.5 10.5L14 14" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"# };
const IMAGE: Icon = Icon { name: "ab-image", view: 16.0, body: r#"<rect x="2" y="3" width="12" height="10" rx="2" stroke="currentColor" stroke-width="1.3" fill="none"/><circle cx="5.6" cy="6.4" r="1" fill="currentColor"/><path d="M2.6 11.4L6.2 8.2l2.3 2.1 1.9-1.6 3 2.7" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round" stroke-linecap="round"/>"# };
const CUBE: Icon = Icon { name: "ab-cube", view: 16.0, body: r#"<path d="M8 1.8l5.4 3v6.4L8 14.2l-5.4-3V4.8L8 1.8zM2.8 4.9L8 8l5.2-3.1M8 8v6" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round"/>"# };
const SOUND: Icon = Icon { name: "ab-sound", view: 16.0, body: r#"<path d="M2.5 6.5v3M5.25 4v8M8 2v12M10.75 4.5v7M13.5 6.5v3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"# };
const GRID: Icon = Icon { name: "ab-grid", view: 16.0, body: r#"<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="9" y="2.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="2.5" y="9" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/><rect x="9" y="9" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" fill="none"/>"# };
const PLAY: Icon = Icon { name: "ab-play", view: 16.0, body: r#"<path d="M5 3.2v9.6L13 8 5 3.2z" fill="currentColor"/>"# };
const PAUSE: Icon = Icon { name: "ab-pause", view: 16.0, body: r#"<path d="M4.5 3h2.5v10H4.5zM9 3h2.5v10H9z" fill="currentColor"/>"# };
pub(crate) const REFRESH: Icon = Icon { name: "ab-refresh", view: 16.0, body: r#"<path d="M13.2 7A5.3 5.3 0 003.4 5.2M2.8 9a5.3 5.3 0 009.8 1.8M3.2 2.6v2.9h2.9M12.8 13.4v-2.9H9.9" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"# };
pub(crate) const TRASH: Icon = Icon { name: "ab-trash", view: 16.0, body: r#"<path d="M2.8 4.3h10.4M6.2 4.1V2.9h3.6v1.2M4 4.5l.6 8.2c0 .5.5.9 1 .9h4.8c.5 0 1-.4 1-.9l.6-8.2" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"# };
pub(crate) const CLOSE: Icon = Icon { name: "ab-close", view: 16.0, body: r#"<path d="M3.8 3.8l8.4 8.4M12.2 3.8l-8.4 8.4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"# };
const SLIDERS: Icon = Icon { name: "ab-sliders", view: 16.0, body: r#"<path d="M2.5 5h6.2M12.2 5h1.3M2.5 11h1.3M7.2 11h6.3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/><circle cx="10.4" cy="5" r="1.8" stroke="currentColor" stroke-width="1.4" fill="none"/><circle cx="5.5" cy="11" r="1.8" stroke="currentColor" stroke-width="1.4" fill="none"/>"# };
pub(crate) const FOLDER: Icon =Icon { name: "ab-folder", view: 16.0, body: r#"<path d="M2 4.5A1.5 1.5 0 013.5 3h2.6l1.5 1.7h4.9A1.5 1.5 0 0114 6.2v5.3a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 11.5v-7z" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linejoin="round"/>"# };
const COPY: Icon = Icon { name: "ab-copy", view: 16.0, body: r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1.5" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M10.5 3.5v-.0A1 1 0 009.5 2.5h-6a1 1 0 00-1 1v6a1 1 0 001 1" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round"/>"# };
const CHECK: Icon = Icon { name: "ab-check", view: 16.0, body: r#"<path d="M3.5 8.5l3 3 6-7" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"# };
const PANEL: Icon = Icon { name: "ab-panel", view: 16.0, body: r#"<rect x="2" y="3" width="12" height="10" rx="2" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M6.2 3.3v9.4" stroke="currentColor" stroke-width="1.3"/>"# };
const GRIP: Icon =Icon { name: "ab-grip", view: 16.0, body: r#"<g fill="currentColor"><circle cx="5.5" cy="3.5" r="1.35"/><circle cx="10.5" cy="3.5" r="1.35"/><circle cx="5.5" cy="8" r="1.35"/><circle cx="10.5" cy="8" r="1.35"/><circle cx="5.5" cy="12.5" r="1.35"/><circle cx="10.5" cy="12.5" r="1.35"/></g>"# };

pub(crate) fn label(s: impl Into<String>, style: TextStyle, c: Color) -> El {
    text(s.into()).font(style).color(c)
}

/// A lifted surface floating on the near-black window.
pub(crate) fn surface(e: El) -> El {
    e.fill(T.panel).radius(PANEL_RADIUS).border(1.0, T.line)
}

pub(crate) fn hairline() -> El {
    el().w_full().h(1.0).fill(T.line).a11y_hidden()
}

/// Two things in the same cell: `on` fades in as `off` fades out, without either moving or
/// changing the size of what holds them.
fn swap(off: El, on: El, showing: bool) -> El {
    let shown = |e: El, visible: bool| e.grid_cell(0).opacity(if visible { 1.0 } else { 0.0 }).scale(if visible { 1.0 } else { 0.86 }).motion(motion::SNAPPY).center_y();
    el().equal_columns(1).child(shown(off, !showing)).child(shown(on, showing))
}

/// A quiet outlined button with an icon, which says so when its action has happened.
pub(crate) fn outlined(name: &str, done: &str, done_now: bool, glyph: &'static Icon, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    row().key(name.to_owned()).role("button").focusable(true).label(name.to_owned()).px(11.0).h(30.0).gap(7.0).radius(CONTROL_RADIUS).items_center()
        .border(1.0, T.line).hover(T.hover).press(T.press).press_scale(0.97).cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(swap(icon(glyph, 14.0).color(T.muted), icon(&CHECK, 14.0).color(T.text), done_now))
        .child(swap(label(name.to_owned(), LABEL, T.text), label(done.to_owned(), LABEL, T.text), done_now))
}

/// An overlay thumb, clear of whatever the list draws beside it.
pub(crate) fn scroll_style() -> ScrollBarStyle {
    ScrollBarStyle::scroll_view(kit())
}

pub(crate) struct Segment<V> {
    name: String,
    count: Option<String>,
    glyph: Option<&'static Icon>,
    value: V,
}

pub(crate) fn plain<V>(name: impl Into<String>, value: V) -> Segment<V> {
    Segment { name: name.into(), count: None, glyph: None, value }
}

/// A row of mutually exclusive choices inside a hairline. The current one is raised.
pub(crate) fn segments<V: Copy + PartialEq + 'static>(key: &str, items: Vec<Segment<V>>, current: V, on: impl Fn(&mut App, V, &mut Tx) + Clone + 'static) -> El {
    let mut bar = row().key(key.to_owned()).role("radiogroup").h(CONTROL_H).radius(GROUP_RADIUS).border(1.0, T.line).padding(GROUP_PAD).equal_columns(items.len());
    // The indicator shares the chosen segment's cell, so it slides when the choice moves.
    if let Some(at) = items.iter().position(|i| i.value == current) {
        bar = bar.child(el().key("indicator").grid_cell(at).h(SEGMENT_H).radius(SEGMENT_RADIUS).fill(T.raised).lead(motion::LEAD, motion::DEFAULT).a11y_hidden());
    }
    for (at, Segment { name, count, glyph, value }) in items.into_iter().enumerate() {
        let selected = value == current;
        let on = on.clone();
        let whole = count.as_ref().map_or_else(|| name.clone(), |c| format!("{name} {c}"));
        let mut item = row().role("radio").checked(selected).label(whole.clone()).focusable(true).key(whole).grid_cell(at).px(10.0).h(SEGMENT_H).gap(6.0).radius(SEGMENT_RADIUS).center()
            .hover(if selected { Color::TRANSPARENT } else { T.hover })
            .press(T.press)
            .press_scale(0.96)
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| on(s, value, tx));
        if let Some(g) = glyph { item = item.child(icon(g, 14.0).color(if selected { T.text } else { T.muted })); }
        item = item.child(label(name, LABEL, if selected { T.text } else { T.muted }));
        // Name to the left, count to the right edge, as the pack rows do. The slot is a fixed
        // width, so a changing number can't move anything or unbalance the tab.
        if let Some(c) = count {
            item = item.child(el().grow(1.0)).child(row().w(COUNT_SLOT).justify_end().child(label(c, SMALL, if selected { T.muted } else { T.faint })));
        }
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
    // The tabs share equal columns, and "Textures" with its count slot is always the widest.
    // Padding, icon and its gap, the name; then the spacer's gap and the slot's.
    let count = if s.is_compact() { 0.0 } else { 6.0 + 6.0 + COUNT_SLOT };
    GROUP_PAD * 2.0 + (20.0 + 20.0 + 52.0 + count) * 4.0
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

/// Keys in the search box: Escape leaves it, and the rest work the list under it.
fn search_key(s: &mut App, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
    use rune::Key;
    let count = s.suggestions().len();
    match event.key {
        Key::Escape => tx.blur(),
        Key::Down if count > 0 => s.step_suggestion(1, count),
        Key::Up if count > 0 => s.step_suggestion(-1, count),
        Key::Tab if count > 0 => s.accept_suggestion(s.suggestion_at.unwrap_or(0), tx),
        Key::Enter if s.suggestion_at.is_some() => s.accept_suggestion(s.suggestion_at.unwrap_or(0), tx),
        _ => return false,
    }
    true
}

fn search_box(s: &App) -> El {
    let field = TextInput::new(&s.search)
        .placeholder("Search assets")
        .label("Search assets")
        .plain()
        .width(None)
        .on_edit(|s: &mut App| &mut s.search)
        .on_change(|s: &mut App, tx| s.refilter(tx))
        .on_key(search_key);
    let empty = s.search.text.is_empty();
    let bar = row().key("searchbar").w(SEARCH_W).h(CONTROL_H).pl(11.0).pr(GROUP_PAD).gap(4.0).radius(GROUP_RADIUS).items_center().border(1.0, T.line).cursor(Cursor::Text)
        .child(icon(&SEARCH, 15.0).color(T.muted))
        .child(el().grow(1.0).min_w(0.0).child(field.build(kit()).key("search")));
    // The end of the bar holds the shortcut hint, or the way back to it, in the same slot.
    if empty {
        bar.child(el().size(SEGMENT_H, SEGMENT_H).radius(SEGMENT_RADIUS).center().fill(T.raised).a11y_hidden().child(label("/", SMALL, T.muted)))
    } else {
        bar.child(el().key("search-clear").role("button").focusable(true).label("Clear search").size(SEGMENT_H, SEGMENT_H).radius(SEGMENT_RADIUS).center()
            .hover(T.hover).press(T.press).press_scale(0.92).cursor(Cursor::Pointer)
            .on_press_tx(|s: &mut App, tx| s.clear_search(tx))
            .child(icon(&CLOSE, 12.0).color(T.muted)))
    }
}

/// The list under the search box: what the query's last piece could become, or with an empty
/// box, what can be asked for at all.
fn search_suggestions(s: &App) -> Option<El> {
    let rows = s.suggestions();
    if rows.is_empty() { return None }
    let cheat_sheet = s.search.text.trim().is_empty();
    let mut list = col().key("search-suggestions").w(SEARCH_W).padding(4.0).gap(1.0).radius(WELL_RADIUS + 3.0).fill(T.raised).border(1.0, T.line)
        .shadow(10.0, 28.0, rune::Color::BLACK.alpha(0.35))
        .preserve_focus_on_press(true)
        .anchored("searchbar", rune::Side::Bottom, rune::Align::Start, 6.0);
    for (i, row_data) in rows.iter().enumerate() {
        let picked = s.suggestion_at == Some(i);
        let shown = row_data.insert.trim_end().trim_matches('-').to_owned();
        let line = row().key(format!("suggest-{i}")).role("button").label(shown.clone()).w_full().h(28.0).px(8.0).gap(12.0).radius(SEGMENT_RADIUS).items_center().cursor(Cursor::Pointer)
            .fill(if picked { T.press } else { rune::Color::TRANSPARENT })
            .hover(T.hover)
            .on_press_tx(move |s: &mut App, tx| s.accept_suggestion(i, tx))
            .child(el().shrink().min_w(0.0).child(text(shown).font(BODY).color(T.text).truncate()))
            .child(el().grow(1.0).min_w(0.0))
            .child(el().shrink().min_w(0.0).child(label(row_data.hint, SMALL, T.muted).truncate()));
        list = list.child(line);
    }
    if cheat_sheet {
        list = list.child(hairline().mt(3.0).mb(3.0))
            .child(el().px(8.0).pb(4.0).pt(2.0).child(label("\"exact words\"   -leave out   this|that", SMALL, T.muted)));
    }
    Some(list)
}

/// The window's own title bar: a band that drags the window, holding the app's main controls.
/// Rune draws the caption buttons over its trailing edge and keeps Windows' snap behaviour.
fn titlebar(s: &App, cx: &mut Cx<App>) -> El {
    let scope = s.filter.pack.and_then(|p| s.lib.packs.get(p as usize)).map_or("All packs", |p| p.name.as_str());
    let sorts = vec![plain("Pack", Sort::Pack), plain("Name", Sort::Name), plain("Largest", Sort::Largest), plain("Newest", Sort::Newest)];
    let mut title = row().grow(1.0).min_w(0.0).gap(10.0).items_end()
        .child(el().min_w(0.0).child(text(scope.to_owned()).key("scope").font(TITLE).color(T.text).truncate()));
    if !s.is_compact() {
        let count = commas(s.results.ids.len());
        let summary = s.results.corrected.as_ref().map_or_else(|| format!("{count} assets"), |word| format!("{count} assets for \u{201c}{word}\u{201d}"));
        title = title.child(label(summary, SMALL, T.muted).mb(2.0));
    }
    let toggle_panel = el().key("sidebar-toggle").role("button").focusable(true).label("Toggle library panel").help("Toggle library panel (Ctrl+B)").size(30.0, 30.0).radius(CONTROL_RADIUS).center()
        .hover(T.hover).press(T.press).press_scale(0.94).cursor(Cursor::Pointer)
        .on_press_tx(|s: &mut App, tx| s.toggle_sidebar(tx))
        .child(icon(&PANEL, 16.0).color(T.muted));
    let mark = el().key("app-mark").size(26.0, 26.0).radius(CONTROL_RADIUS).center().fill(T.accent).a11y_hidden()
        .child(icon(&GRID, 16.0).color(T.on_accent));
    cx.caption_color(T.muted);
    let insets = cx.chrome_insets();
    row().key("titlebar").w_full().h(TITLEBAR_H).window_drag().pl(insets.leading + 11.0).pr(insets.trailing + 12.0).gap(8.0).items_center()
        .child(mark)
        .child(toggle_panel.ml(4.0))
        .child(title.ml(2.0))
        .child(segments("sort", sorts, s.filter.sort, |s, v, tx| s.set_sort(v, tx)))
        .child(search_box(s))
        .child(el().key("settings-button").role("button").focusable(true).label("Settings").help("Settings (Ctrl+,)").size(30.0, 30.0).radius(CONTROL_RADIUS).center()
            .hover(T.hover).press(T.press).press_scale(0.94).cursor(Cursor::Pointer)
            .on_press_tx(|s: &mut App, _| s.open_settings())
            .child(icon(&SLIDERS, 16.0).color(T.muted)))
}

fn chip(key: String, name: String, count: Option<String>, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    let mut c = row().key(key.clone()).role("button").focusable(true).label(key).px(9.0).h(SEGMENT_H).gap(5.0).radius(SEGMENT_RADIUS).items_center()
        .fill(if on { T.raised } else { Color::TRANSPARENT })
        .hover(if on { Color::TRANSPARENT } else { T.hover })
        .press(T.press)
        .press_scale(0.96)
        .cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(label(name, SMALL, if on { T.text } else { T.muted }));
    if let Some(n) = count { c = c.child(label(n, SMALL, if on { T.muted } else { T.faint })); }
    c
}

/// An on/off control, drawn so its state reads without comparing it to its neighbours.
fn toggle(name: &str, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    row().key(name.to_owned()).role("switch").checked(on).focusable(true).label(name.to_owned()).px(10.0).h(CONTROL_H).gap(7.0).radius(CONTROL_RADIUS).items_center()
        .border(1.0, T.line)
        .hover(T.hover)
        .press(T.press)
        .press_scale(0.97)
        .cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(checkbox(on))
        .child(label(name.to_owned(), LABEL, if on { T.text } else { T.muted }))
}

/// A checkbox that's always there, so turning it on moves nothing.
pub(crate) fn checkbox(on: bool) -> El {
    let tick = icon(&CHECK, 10.0).color(T.on_accent).opacity(if on { 1.0 } else { 0.0 }).scale(if on { 1.0 } else { 0.6 }).motion(motion::SNAPPY);
    el().key("check").size(14.0, 14.0).radius(4.0).center().motion(motion::SNAPPY)
        .fill(if on { T.accent } else { Color::TRANSPARENT })
        .border(1.0, if on { T.accent } else { T.faint })
        .child(tick)
}

/// Asks for a new library folder and switches to it.
pub(crate) fn pick_library_folder(tx: &mut Tx) {
    tx.open_folder(rune::FileDialogOptions { title: "Choose a library folder".into(), ..Default::default() }, |s: &mut App, picked, _| {
        if let Ok(Some(path)) = picked { s.change_root(path); }
    });
}

/// A chip that's its own label.
fn tool(name: &str, on: bool, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    chip(name.to_owned(), name.to_owned(), None, on, f)
}

fn filter_bar(s: &App) -> El {
    // Whole chips only: the rarest formats go behind "+N" when they don't all fit.
    let chip_width = |e: &str| 18.0 + e.len() as f32 * 7.0 + 2.0;
    const MORE_WIDTH: f32 = 42.0;
    // What's left beside the tabs: the row's padding, the gap and the group's own inset come
    // out of it too.
    let mut room = s.grid_width - GRID_PAD * 2.0 - 12.0 - kind_tabs_width(s) - GROUP_PAD * 2.0;
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
    let mut filters = row().key("filters").w_full().py(12.0).px(GRID_PAD).gap(12.0).items_center().child(kind_tabs(s));
    if !all.is_empty() { filters = filters.child(formats.key("format-group").h(CONTROL_H).radius(GROUP_RADIUS).border(1.0, T.line).padding(GROUP_PAD)); }
    filters
}

fn sidebar(s: &App) -> El {
    // Rows stop short of the panel's edge, leaving the scroll thumb a clear lane of its own.
    let mut list = col().key("packlist").w_full().pl(PACK_LEFT).pr(PACK_RIGHT).pb(8.0).gap(PACK_GAP).instant_changes();
    let row_for = |name: String, count: usize, on: bool, pack: Option<u32>| {
        row().key(name.clone()).role("button").focusable(true).label(name.clone()).w_full().h(PACK_ROW).px(10.0).radius(CONTROL_RADIUS).items_center().gap(8.0)
            .hover(if on { Color::TRANSPARENT } else { T.hover })
            .press(T.press)
            .press_scale(0.985)
            .cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.set_pack(pack, tx))
            .child(el().grow(1.0).min_w(0.0).child(text(name).font(BODY).color(if on { T.text } else { T.muted }).truncate()))
            .child(label(commas(count), SMALL, if on { T.muted } else { T.faint }))
    };
    list = list.child(row_for("All packs".to_owned(), s.results.any_pack as usize, s.filter.pack.is_none(), None));
    let (mut shown, mut active) = (1usize, 0usize);
    for (i, pack) in s.lib.packs.iter().enumerate() {
        let i = i as u32;
        let count = s.results.by_pack.get(i as usize).copied().unwrap_or(0) as usize;
        // A filter's dead ends only add noise; the selected pack stays so it can be cleared.
        if count == 0 && s.filter.pack != Some(i) { continue }
        if s.filter.pack == Some(i) { active = shown; }
        shown += 1;
        list = list.child(row_for(pack.name.clone(), count, s.filter.pack == Some(i), Some(i)));
    }
    // One pill that slides to the chosen pack.
    let pill = el().key("pack-pill").pin(Some(PACK_LEFT), Some(active as f32 * (PACK_ROW + PACK_GAP)), Some(PACK_RIGHT), None).h(PACK_ROW).radius(CONTROL_RADIUS).fill(T.raised)
        .lead(motion::LEAD, motion::DEFAULT).a11y_hidden();
    let scroller = col().key("packs").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false })
        .on_wheel(|s: &mut App, e, tx| s.packs_glide.wheel("packs", e, tx))
        .on_scroll_offset(s.packs_glide.watch())
        .child(col().w_full().child(pill).child(list)).scroll_bar_styled(scroll_style());
    let open = s.sidebar_open;
    let mut panel = surface(el().key("sidebar").w(if open { SIDEBAR_W } else { 0.0 }).h_full().min_h(0.0)).opacity(if open { 1.0 } else { 0.0 }).clip_soft(0.0);
    if !open { panel = panel.inert(); }
    panel.child(col().w(SIDEBAR_W).h_full().min_h(0.0)
        .child(el().w_full().grow(1.0).min_h(0.0).pt(PACK_LEFT).child(scroller))
    )
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
    let (px, fade) = s.thumbs.tile(asset.key);
    let inner = width - TILE_PAD * 2.0;
    // A waveform is drawn to its edges, so it's the one thumbnail that needs a margin.
    let (image_w, image_h) = if asset.kind == Kind::Audio { (inner - 24.0, (s.picture_height() - 14.0).max(1.0)) } else { (inner, s.picture_height()) };
    let image = el().w(image_w).h(image_h).radius(WELL_RADIUS).pixels_fit(&px, SurfaceFit::Fit);
    let image = match s.motion.speed() {
        off if off <= 0.0 => image.live_opacity(&fade),
        speed => image.live_opacity(fade.smooth(Spring::new(THUMB_FADE_SECONDS / speed, 0.0))),
    };
    let mut picture = el().w(inner).h(s.picture_height()).radius(WELL_RADIUS).fill(T.well).center().child(image);
    if failed { picture = picture.child(icon(kind_icon(asset.kind), 22.0).color(T.faint)); }
    // Lifts the well under the pointer too, without the view running for every tile crossed.
    picture = picture.child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).radius(WELL_RADIUS).fill(T.well_hover).border(1.0, T.well_edge).inert().group_opacity(0.0, 1.0));
    let max = s.drag_max;
    col().key(format!("a{id}")).role("button").label(asset.name().to_owned()).w(width).padding(TILE_PAD).gap(TILE_GAP).radius(TILE_RADIUS).cursor(Cursor::Grab).hover_group()
        // Filters swap tiles at once; only their colours ease.
        .transition(Transition::none())
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
    let version = (s.generation, cols, width.to_bits(), selected, drag, any_facts, s.drag_max, is_dark(), s.motion);
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
        self.grid_width < 720.0
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
        .on_visible_rows(|s: &mut App, range| { s.show_rows(range); false });
    let empty = s.results.ids.is_empty() && !s.scanning;
    let body = col().key("grid").w_full().grow(1.0).min_h(0.0).pt(GRID_INSET).instant_changes()
        .scroll(ScrollSpec { stick_bottom: false })
        .on_wheel(|s: &mut App, e, tx| s.grid_glide.wheel("grid", e, tx))
        .on_scroll_offset(s.grid_glide.watch())
        .child(rows).scroll_bar_styled(scroll_style());
    let mut pane = col().w_full().grow(1.0).min_h(0.0).on_layout(|s: &mut App, r: [f32; 4]| {
        if (r[2] - s.grid_width).abs() > 0.5 { s.set_grid_width(r[2]); }
    });
    if empty && !s.lib.assets.is_empty() {
        let (made, total) = s.thumbs.progress();
        let message = if s.results.uses_facts && made < total {
            "Nothing matches yet. Sizes and lengths are still being read."
        } else {
            "Nothing matches. Remove a word or clear the filters."
        };
        pane = pane.center().child(label(message, BODY, T.muted));
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
    // The idle hint only shows when all of it fits; progress always shows.
    bar = if working || s.grid_width >= 960.0 {
        bar.child(text("Making thumbnails: 00,000 of 00,000").live_text(&s.status).font(SMALL).color(T.muted).truncate_fade().grow(1.0).min_w(0.0))
    } else {
        bar.child(el().grow(1.0))
    };
    if let Some(m) = &s.message {
        bar = bar.child(el().key(format!("message-{m}")).min_w(0.0).transition(Transition::offset(0.0, 5.0).combined(Transition::opacity()))
            .child(text(m.clone()).key("message").font(SMALL).color(T.text).truncate()));
    }
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
        .child(el().w(side).h(side).radius(6.0).center().fill(T.raised).hover(T.hover).press(T.press).motion(motion::SNAPPY).border(1.0, if on { T.accent } else { T.edge_strong })
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
            if matches!(asset.ext, "obj" | "dae" | "fbx") {
                actions.push(row().w_full().justify_between().items_center()
                    .child(label("Material and texture files", SMALL, T.muted))
                    .child(segments("companions", vec![plain("Include", true), plain("Leave out", false)], s.companions, |s, v, _| s.companions = v)));
            }
            Pane { tools: None, stage: model_stage_handlers(stage_el()), hint: Some("Drag to orbit, right-drag to pan, scroll to zoom"), corner: Some(tool("Reset view", false, |s, _| s.reset_view())), dock: None, actions }
        }
        Preview::Audio { seconds, rate, channels } => {
            let playing = s.showing_playing;
            let wave = el().key("waveform").w_full().h(200.0).cursor(Cursor::Pointer)
                .pixels(&s.wave)
                .on_layout(|s: &mut App, r: [f32; 4]| s.stage_size = [r[2], r[3]])
                .on_drag(|s: &mut App, drag, tx| {
                    if drag.phase != Phase::Up { s.seek_fraction(drag.x / drag.w.max(1.0), tx); }
                })
                .child(el().key("playhead").w(1.5).h_full().fill(T.accent).absolute(0.0, 0.0).live_offset(&s.playhead));
            let dock = row().key("tools").w_full().gap(10.0).items_center()
                .child(el().key("play").role("button").label(if playing { "Pause" } else { "Play" }).focusable(true).size(30.0, 30.0).radius(15.0).center().fill(T.accent).press_scale(0.9).cursor(Cursor::Pointer)
                    .on_press_tx(|s: &mut App, tx| s.toggle_playback(tx))
                    .child(swap(icon(&PLAY, 14.0).color(T.on_accent), icon(&PAUSE, 14.0).color(T.on_accent), playing)))
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
    // Selecting within a kind swaps at once; arriving at a different kind fades the new layout in.
    let kind_key = match asset.kind { Kind::Texture => "texture-pane", Kind::Model => "model-pane", Kind::Audio => "audio-pane" };
    let mut body = col().key(kind_key).w_full().padding(14.0).gap(12.0).min_h(0.0).grow(1.0).instant_changes()
        .transition(Transition::asymmetric(Transition::opacity(), Transition::none()));
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
    if let (Preview::Model(m), Some(file)) = (&s.preview, s.current_guess()) {
        let name = file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let more = m.guessed.len() - 1;
        facts.push(("Texture guessed", if more > 0 { format!("{name} +{more}") } else { name }));
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
        .child(outlined("Show in Explorer", "Opened", s.is_confirmed("reveal"), &FOLDER, move |s, tx| {
            s.reveal(id);
            s.confirm("reveal", tx);
        }))
        .child(outlined("Copy path", "Copied", s.is_confirmed("copy"), &COPY, move |s, tx| {
            tx.copy(path.clone());
            s.confirm("copy", tx);
        })));
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

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let dark = match s.theme {
        Theme::System => cx.system_dark(),
        Theme::Light => false,
        Theme::Dark => true,
    };
    set_dark(dark);
    s.resolved_dark.set(dark);
    match s.motion {
        // Rune still gives colours a short fade under reduced motion; run its clock flat out
        // so "Off" really is off. Windows' own animation setting still applies to the others.
        Motion::Off => {
            cx.reduced_motion(true);
            cx.time_scale(8.0);
        }
        speed => cx.time_scale(speed.speed()),
    }
    let main = surface(col().key("main").grow(1.0).h_full().min_w(0.0).min_h(0.0))
        .child(filter_bar(s))
        .child(hairline())
        .child(grid(s))
        .child(hairline())
        .child(status_bar(s));
    let panels = row().key("panels").w_full().grow(1.0).min_h(0.0).px(WINDOW_GAP).pb(WINDOW_GAP)
        .child(sidebar(s))
        .child(el().key("sidebar-gap").w(if s.sidebar_open { WINDOW_GAP } else { 0.0 }).h_full())
        .child(main)
        .child(inspector_handle())
        .child(inspector(s));
    col().key("root").w_full().h_full().fill(T.bg)
        .on_layout(|s: &mut App, r: [f32; 4]| { s.window_width = r[2]; s.window_height = r[3]; })
        .child(titlebar(s, cx))
        .child(panels)
        .children(search_suggestions(s))
        .children(s.settings_open.then(|| settings::sheet(s)))
}

pub fn on_key(s: &mut App, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
    use rune::Key;
    if event.is_command(',') {
        if s.settings_open { s.settings_open = false; } else { s.open_settings(); }
        return true;
    }
    // Settings is modal: the grid doesn't hear keys underneath it.
    if s.settings_open { return false }
    if event.is_command('b') {
        s.toggle_sidebar(tx);
        return true;
    }
    if tx.focused().is_some_and(|f| f.contains("search")) { return false }
    let cols = s.columns.max(1) as isize;
    match &event.key {
        Key::Left => s.step_selection(-1, tx),
        Key::Right => s.step_selection(1, tx),
        Key::Up => s.step_selection(-cols, tx),
        Key::Down => s.step_selection(cols, tx),
        Key::Text(t) if t == " " => s.toggle_playback(tx),
        Key::Text(t) if t == "/" => s.search.focused = true,
        Key::Escape if !s.search.text.is_empty() => {
            s.search = rune::TextEdit::new("");
            s.refilter(tx);
        }
        _ => return false,
    }
    true
}
