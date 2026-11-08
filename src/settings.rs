//! The settings sheet: every preference the app keeps, grouped by what it affects.

use rune::tok::motion;
use rune::{Color, Cursor, El, Phase, ScrollSpec, Transition, col, el, icon, row, text};
use rune_kit::ScrollBar;

use crate::app::{App, Theme, bytes};
use crate::ui::{BODY, CLOSE, FOLDER, NUMBERS, PANEL_RADIUS, REFRESH, SMALL, T, TITLE, TRASH, WELL_RADIUS, checkbox, hairline, label, outlined, pick_library_folder, plain, scroll_style, segments, surface};

const SHEET_W: f32 = 560.0;
const HEADER_H: f32 = 56.0;
/// The sheet keeps this much of the window around it.
const SHEET_MARGIN: f32 = 72.0;

/// A named group of settings in one hairline card.
fn section(title: &str, rows: Vec<El>) -> El {
    let mut card = col().w_full().radius(WELL_RADIUS + 1.0).border(1.0, T.line);
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 { card = card.child(hairline()); }
        card = card.child(r);
    }
    col().w_full().gap(7.0).child(label(title.to_owned(), SMALL, T.muted)).child(card)
}

/// One setting: what it is on the left, its control on the right.
fn setting(name: &str, detail: Option<String>, control: El) -> El {
    let mut about = col().grow(1.0).min_w(0.0).gap(1.0).child(label(name.to_owned(), BODY, T.text));
    if let Some(d) = detail { about = about.child(text(d).font(SMALL).color(T.muted).truncate_head().w_full()); }
    row().key(name.to_owned()).w_full().min_h(46.0).px(13.0).py(8.0).gap(16.0).items_center().child(about).child(control)
}

/// A setting that's a single on/off, where the whole row is the control.
fn switch_row(name: &str, detail: &str, on: bool, f: impl Fn(&mut App) + 'static) -> El {
    setting(name, Some(detail.to_owned()), checkbox(on)).role("switch").checked(on).label(name.to_owned()).focusable(true)
        .hover(T.hover).press(T.press).cursor(Cursor::Pointer).on_press(f)
}

fn volume_slider(s: &App) -> El {
    const W: f32 = 168.0;
    const KNOB: f32 = 14.0;
    let v = s.volume;
    let track = el().w_full().h(4.0).radius(2.0).fill(T.line).child(el().w_frac(v).h_full().radius(2.0).fill(T.accent));
    row().gap(10.0).items_center()
        .child(el().key("volume").role("slider").label("Volume").range_value(f64::from(v), 0.0, 1.0).focusable(true).w(W).h(22.0).center_y().cursor(Cursor::Pointer)
            .on_step(|s: &mut App, steps, _| s.set_volume(s.volume + steps as f32 * 0.05))
            .on_drag(|s: &mut App, drag, _| {
                s.set_volume(drag.x / drag.w.max(1.0));
                s.volume_dragging = drag.phase != Phase::Up;
            })
            .child(track)
            .child(el().size(KNOB, KNOB).radius(KNOB / 2.0).fill(T.accent).absolute(v * (W - KNOB), 4.0)))
        .child(row().w(34.0).justify_end().child(label(format!("{:.0}%", v * 100.0), NUMBERS, T.muted)))
}

fn storage_detail(amount: Option<u64>, what: &str) -> String {
    amount.map_or_else(|| "Measuring".to_owned(), |n| format!("{} of {what}", bytes(n)))
}

pub fn sheet(s: &App) -> El {
    let appearance = section("Appearance", vec![
        setting("Theme", None, segments("theme", vec![plain("System", Theme::System), plain("Light", Theme::Light), plain("Dark", Theme::Dark)], s.theme, |s, v, tx| s.set_theme(v, tx))),
    ]);

    let library = section("Library", vec![
        setting("Folder", Some(s.library_folder()), outlined("Change", "Change", false, &FOLDER, |_, tx| pick_library_folder(tx))),
        setting("Check for new files", Some("Happens at every launch, and here on demand".to_owned()), outlined("Rescan", "Scanning", s.scanning, &REFRESH, |s, _| s.rescan())),
        switch_row("Show every format of a model", "When a pack ships it as GLB, OBJ, DAE and FBX", s.filter.all_formats, |s| s.toggle_all_formats_now()),
    ]);

    let previews = section("Previews", vec![
        setting("Volume", None, volume_slider(s)),
        switch_row("Play sounds on select", "Start a sound as soon as it's selected", s.autoplay, |s| s.autoplay = !s.autoplay),
        setting("Texture display", Some("How a preview draws when zoomed in".to_owned()), segments("texture-display", vec![plain("Pixels", false), plain("Smooth", true)], s.stage.view.smooth, |s, v, _| s.stage.view.smooth = v)),
    ]);

    let export = section("Dragging out", vec![
        setting("Texture size", Some("The largest side when dragging from the grid".to_owned()), segments("default-drag", vec![plain("Full", None), plain("1024", Some(1024u32)), plain("512", Some(512)), plain("256", Some(256))], s.drag_max, |s, v, _| s.drag_max = v)),
        setting("Resize filter", Some("Hard pixels keeps pixel art crisp".to_owned()), segments("default-filter", vec![plain("Smooth", false), plain("Hard pixels", true)], s.nearest, |s, v, _| s.nearest = v)),
        switch_row("Bring model files along", "OBJ, DAE and FBX keep their materials and textures", s.companions, |s| s.companions = !s.companions),
    ]);

    let (thumbs, staged) = s.storage.map_or((None, None), |(t, d)| (Some(t), Some(d)));
    let storage = section("Storage", vec![
        setting("Thumbnails", Some(storage_detail(thumbs, "cached previews")), outlined("Rebuild", "Rebuilding", s.is_confirmed("rebuild"), &REFRESH, |s, tx| {
            s.rebuild_thumbnails();
            s.confirm("rebuild", tx);
        })),
        setting("Dragged copies", Some(storage_detail(staged, "files prepared for dragging")), outlined("Clear", "Cleared", s.is_confirmed("clear"), &TRASH, |s, tx| {
            s.clear_staged();
            s.confirm("clear", tx);
        })),
    ]);

    let body = col().key("settings-body").w_full().max_h((s.window_height - SHEET_MARGIN - HEADER_H).max(200.0)).scroll(ScrollSpec { stick_bottom: false })
        .on_wheel(|s: &mut App, e, tx| s.settings_glide.wheel("settings-body", e, tx))
        .on_scroll_offset(s.settings_glide.watch())
        .child(col().w_full().px(16.0).pb(16.0).gap(16.0).child(appearance).child(library).child(previews).child(export).child(storage))
        .scroll_bar_styled(scroll_style());

    let header = row().w_full().h(HEADER_H).pl(18.0).pr(12.0).items_center().justify_between()
        .child(text("Settings").font(TITLE).color(T.text))
        .child(el().key("settings-close").role("button").focusable(true).label("Close settings").size(30.0, 30.0).radius(7.0).center()
            .hover(T.hover).press(T.press).press_scale(0.94).cursor(Cursor::Pointer)
            .on_press(|s: &mut App| s.settings_open = false)
            .child(icon(&CLOSE, 15.0).color(T.muted)));

    let card = surface(col().key("settings").role("dialog").label("Settings").w(SHEET_W).radius(PANEL_RADIUS + 2.0))
        .modal(true, |s: &mut App| s.settings_open = false)
        .shadow(18.0, 48.0, Color::BLACK.alpha(0.4))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .motion(motion::SNAPPY)
        .child(header)
        .child(body);

    el().key("settings-scrim").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().fill(Color::BLACK.alpha(0.5))
        .transition(Transition::opacity())
        .child(card)
}
