//! The inspector's Edit panel. Hidden until asked for, then a fixed-height drawer of controls
//! that takes its room from the preview, and a readout of "what will I get, and how big is it"
//! in the place the file details were. Controls that don't apply dim instead of leaving, so
//! nothing around them moves.

use rune::tok::motion;
use rune::{Color, Cursor, El, Phase, ScrollSpec, Tx, col, el, row, text};
use rune_kit::ScrollBar;

use crate::app::{App, bytes, ladder};
use crate::edit::mesh::{MaterialInfo, ModelDoc};
use crate::edit::{Decoded, Effort, ImageEdit, MaterialEdit, ModelEdit};
use crate::editor::{AudioTab, Marker};
use crate::library::{Asset, Kind};
use crate::ui::{LABEL, PLAY, REFRESH, SMALL, T, WELL_RADIUS, color, dim, label, layer, outlined, plain, scroll_style, segments, segments_inset, toggle};

const NAME_W: f32 = 74.0;
const VALUE_W: f32 = 42.0;
const KNOB: f32 = 13.0;
const SLIDER_H: f32 = 24.0;
/// The drawer's distance from the preview above it.
const SLOT_GAP: f32 = 12.0;
const CARD_PAD: f32 = 10.0;
/// Three rows of 30-point controls.
const IMAGE_PANEL_H: f32 = 128.0;
/// The tab bar over the tallest tab, Loop.
const AUDIO_PANEL_H: f32 = 240.0;
const MATERIAL_ROW_GAP: f32 = 4.0;
const SEAM_H: f32 = 64.0;

fn clock_fine(seconds: f32) -> String {
    let millis = (seconds * 1000.0).round() as u32;
    format!("{}:{:02}.{:03}", millis / 60_000, millis / 1000 % 60, millis % 1000)
}

/// A bare slider track. `on` gets a 0..1 value and whether the gesture is over.
fn track(key: String, name: &str, value: f32, fill: Color, on: impl Fn(&mut App, f32, bool) + Clone + 'static) -> El {
    let value = value.clamp(0.0, 1.0);
    let on_step = on.clone();
    let bar = el().w_full().h(4.0).radius(2.0).fill(T.line).child(el().w_frac(value).h_full().radius(2.0).fill(fill));
    // The knob sits between two springs, so it needs no measurements.
    let knob = row().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).items_center().inert()
        .child(el().grow(value.max(0.0001)))
        .child(el().size(KNOB, KNOB).radius(KNOB / 2.0).fill(fill))
        .child(el().grow((1.0 - value).max(0.0001)));
    el().key(key).role("slider").label(name.to_owned()).range_value(f64::from(value), 0.0, 1.0).focusable(true).grow(1.0).min_w(0.0).h(SLIDER_H).center_y().cursor(Cursor::Pointer)
        .on_step(move |s: &mut App, steps, _| on_step(s, (value + steps as f32 * 0.02).clamp(0.0, 1.0), true))
        .on_drag(move |s: &mut App, drag, tx| {
            let at = ((drag.x - KNOB * 0.5) / (drag.w - KNOB).max(1.0)).clamp(0.0, 1.0);
            on(s, at, drag.phase == Phase::Up);
            // A knob stays under the pointer, rather than springing after it.
            tx.instant();
        })
        .child(bar)
        .child(knob)
}

fn field_name(name: &str) -> El {
    el().w(NAME_W).child(label(name.to_owned(), SMALL, T.muted))
}

fn slider(key: &str, name: &str, value: f32, shown: String, on: impl Fn(&mut App, f32, bool) + Clone + 'static) -> El {
    row().w_full().gap(8.0).items_center()
        .child(field_name(name))
        .child(track(key.to_owned(), name, value, T.accent, on))
        .child(row().w(VALUE_W).justify_end().child(label(shown, LABEL, T.muted)))
}

fn labelled(name: &str, control: El) -> El {
    row().w_full().gap(8.0).items_center().child(field_name(name)).child(el().grow(1.0).min_w(0.0).child(control))
}

/// A small square button with a one-character face.
fn step_button(key: String, face: &str, help: &str, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    el().key(key).role("button").focusable(true).label(help.to_owned()).help(help.to_owned()).size(24.0, 24.0).radius(6.0).center().border(1.0, T.line)
        .hover(T.hover).press(T.press).press_scale(0.94).cursor(Cursor::Pointer)
        .on_press_tx(f)
        .child(label(face.to_owned(), LABEL, T.text))
}

fn marker_row(name: &str, frame: u64, rate: u32, marker: Marker) -> El {
    let nudge = move |sign: f32| move |s: &mut App, tx: &mut Tx| {
        let amount = if tx.mods().shift { 10.0 } else { 1.0 };
        s.nudge_marker(marker, sign * amount);
    };
    row().w_full().gap(8.0).items_center()
        .child(field_name(name))
        .child(step_button(format!("{name}-earlier"), "\u{2212}", "1 ms earlier, Shift for 10 ms", nudge(-1.0)))
        .child(row().grow(1.0).min_w(0.0).justify_center().child(label(clock_fine(frame as f32 / rate.max(1) as f32), LABEL, T.text)))
        .child(step_button(format!("{name}-later"), "+", "1 ms later, Shift for 10 ms", nudge(1.0)))
}

fn trim_tab(start: u64, end: u64, frames: u64, rate: u32) -> El {
    let length = (end - start) as f32 / rate.max(1) as f32;
    let cut = start > 0 || end < frames;
    col().w_full().gap(6.0)
        .child(marker_row("Start", start, rate, Marker::Start))
        .child(marker_row("End", end, rate, Marker::End))
        .child(row().w_full().items_center().justify_between()
            .child(label(format!("Keeping {}", clock_fine(length)), SMALL, T.muted))
            .child(dim(outlined("Whole file", "Whole file", false, &REFRESH, |s, _| s.clear_section()), cut)))
}

fn loop_tab(s: &App) -> El {
    let edit = s.audio_edit();
    let strip = el().key("seam").w_full().h(SEAM_H).radius(6.0).fill(T.bg).cursor(Cursor::ResizeColumn)
        .help(format!("Showing {:.0} ms either side of the join. Drag to slide the end, scroll to zoom.", s.seam_window_ms()))
        .pixels(&s.edit.seam)
        .on_drag(|s: &mut App, drag, _| s.drag_seam(drag))
        .on_wheel(|s: &mut App, wheel, _| { s.zoom_seam(wheel.dy); true })
        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().inert().child(el().w(1.0).h_full().fill(T.accent.alpha(0.55))));
    let fades = vec![plain("Off", 0u32), plain("10", 10), plain("50", 50), plain("200", 200), plain("1000", 1000)];
    let tools = col().w_full().gap(8.0)
        .child(strip)
        .child(labelled("Fade (ms)", segments_inset("crossfade", fades, edit.crossfade_ms, |s, v, _| {
            let mut edit = s.audio_edit();
            edit.crossfade_ms = v;
            s.set_audio_edit(edit);
        }, 2.0).w_full()))
        .child(row().w_full().gap(6.0)
            .child(outlined("Find best join", "Searching", s.edit.is_tuning(), &REFRESH, |s, _| s.tune_seam()))
            .child(outlined("Hear the join", "Hear the join", false, &PLAY, |s, tx| s.hear_seam(tx))));
    col().w_full().gap(8.0)
        .child(row().w_full().items_center().justify_between()
            .child(label("Repeat this section", SMALL, T.muted))
            .child(toggle("Loop", edit.looped, |s, _| { let on = !s.audio_edit().looped; s.set_looped(on); })))
        .child(dim(tools, edit.looped))
}

fn compress_tab(s: &App) -> El {
    let edit = s.audio_edit();
    let (source_rate, channels) = s.source_format().unwrap_or((44_100, 2));
    let quality = edit.quality.min(10);
    let mut tab = col().w_full().gap(6.0)
        .child(slider("quality", "Quality", f32::from(quality) / 10.0, format!("q{quality}"), |s, v, done| s.set_quality((v * 10.0).round() as u8, done)));
    let rates: Vec<_> = std::iter::once(plain("Same", None))
        .chain([(32_000u32, "32 k"), (22_050, "22 k"), (11_025, "11 k")].into_iter().filter(|(r, _)| *r < source_rate).map(|(r, name)| plain(name, Some(r))))
        .collect();
    if rates.len() > 1 {
        tab = tab.child(labelled("Sample rate", segments_inset("sample-rate", rates, edit.rate, |s, v, _| {
            let mut edit = s.audio_edit();
            edit.rate = v;
            s.set_audio_edit(edit);
        }, 2.0).w_full()));
    }
    if channels > 1 {
        tab = tab.child(labelled("Channels", segments_inset("channels", vec![plain("Stereo", false), plain("Mono", true)], edit.mono, |s, v, _| {
            let mut edit = s.audio_edit();
            edit.mono = v;
            s.set_audio_edit(edit);
        }, 2.0).w_full()));
    }
    tab
}

fn audio_controls(s: &App) -> El {
    let section = s.audio_section();
    let (start, end, frames) = section.unwrap_or((0, 1, 1));
    let (rate, _) = s.source_format().unwrap_or((1, 1));
    let tabs = vec![plain("Trim", AudioTab::Trim), plain("Loop", AudioTab::Loop), plain("Compress", AudioTab::Compress)];
    let on = s.edit.audio_tab;
    // Every tab is always there in one cell, so changing tab can't change the panel's height.
    let stack = el().key("audio-tabs").w_full().equal_columns(1)
        .child(layer(trim_tab(start, end, frames, rate), on == AudioTab::Trim))
        .child(layer(loop_tab(s), on == AudioTab::Loop))
        .child(layer(compress_tab(s), on == AudioTab::Compress));
    dim(col().w_full().gap(10.0)
        .child(segments("audio-tab", tabs, on, |s, v, _| s.edit.audio_tab = v).w_full())
        .child(stack), section.is_some())
}

fn image_controls(s: &App) -> El {
    let edit = s.image_edit();
    let palettes = vec![plain("All", None), plain("256", Some(256u16)), plain("128", Some(128)), plain("64", Some(64)), plain("32", Some(32)), plain("16", Some(16))];
    let efforts = vec![plain("Fast", Effort::Fast), plain("Balanced", Effort::Balanced), plain("Smallest", Effort::Best)];
    col().w_full().gap(8.0)
        .child(labelled("Colours", segments_inset("colours", palettes, edit.colors, |s, v, _| {
            let edit = ImageEdit { colors: v, ..s.image_edit() };
            s.set_image_edit(edit);
        }, 2.0).w_full()))
        .child(labelled("Effort", segments_inset("effort", efforts, edit.effort, |s, v, _| {
            let edit = ImageEdit { effort: v, ..s.image_edit() };
            s.set_image_edit(edit);
        }, 2.0).w_full()))
        .child(dim(row().w_full().items_center().justify_between()
            .child(label("Hide banding with dots", SMALL, T.muted))
            .child(toggle("Dither", edit.dither, |s, _| {
                let current = s.image_edit();
                s.set_image_edit(ImageEdit { dither: !current.dither, ..current });
            })), edit.colors.is_some()))
}

fn material_summary(doc: &ModelDoc, index: usize, change: &MaterialEdit) -> String {
    let (_, roughness, metallic, _) = doc.values(index, &ModelEdit { materials: [(index, *change)].into() });
    let size = change.max_texture.or_else(|| doc.longest_texture(index)).map_or_else(String::new, |n| format!(", {n}"));
    format!("R {roughness:.2} M {metallic:.2}{size}")
}

fn has_texture_sizes(doc: &ModelDoc, index: usize) -> bool {
    doc.longest_texture(index).is_some_and(|longest| !ladder(longest).is_empty())
}

/// How tall a material's unfolded controls are, known up front so the row can spring to it.
fn material_body_height(doc: &ModelDoc, index: usize, info: &MaterialInfo) -> f32 {
    let mut height = (SLIDER_H + MATERIAL_ROW_GAP) * 3.0; // Roughness, metallic, tint.
    if info.glows() { height += SLIDER_H + MATERIAL_ROW_GAP; }
    if has_texture_sizes(doc, index) { height += 30.0 + MATERIAL_ROW_GAP; }
    if !info.maps.is_empty() { height += 16.0 + MATERIAL_ROW_GAP + 2.0; }
    height + 30.0 + CARD_PAD
}

fn material_body(doc: &ModelDoc, index: usize, info: &MaterialInfo, change: &MaterialEdit) -> El {
    let (shade, roughness, metallic, emissive) = doc.values(index, &ModelEdit { materials: [(index, *change)].into() });
    let mut body = col().w_full().px(CARD_PAD).pb(CARD_PAD).gap(MATERIAL_ROW_GAP)
        .child(slider(&format!("m{index}-rough"), "Roughness", roughness, format!("{roughness:.2}"), move |s, v, done| s.set_material(index, done, |m| m.roughness = Some(v))))
        .child(slider(&format!("m{index}-metal"), "Metallic", metallic, format!("{metallic:.2}"), move |s, v, done| s.set_material(index, done, |m| m.metallic = Some(v))));
    if info.glows() {
        let peak = emissive.iter().copied().fold(0.0, f32::max);
        body = body.child(slider(&format!("m{index}-glow"), "Emissive", peak, format!("{peak:.2}"), move |s, v, done| s.set_material(index, done, |m| m.emissive = Some(v))));
    }
    let channel = |at: usize, fill: &str| {
        track(format!("m{index}-tint-{at}"), ["Red", "Green", "Blue"][at], shade[at], color(fill), move |s: &mut App, v, done| {
            s.set_material(index, done, |m| {
                let mut c = m.color.unwrap_or([shade[0], shade[1], shade[2]]);
                c[at] = v;
                m.color = Some(c);
            });
        })
    };
    body = body.child(row().w_full().gap(8.0).items_center().child(field_name("Tint")).child(row().grow(1.0).min_w(0.0).gap(6.0)
        .child(channel(0, "#E5484D")).child(channel(1, "#46A758")).child(channel(2, "#3E7BFA"))));
    if has_texture_sizes(doc, index) {
        let longest = doc.longest_texture(index).unwrap_or(0);
        let items = std::iter::once(plain("Full", None)).chain(ladder(longest).into_iter().map(|n| plain(n.to_string(), Some(n)))).collect();
        body = body.child(labelled("Textures", segments_inset(&format!("m{index}-size"), items, change.max_texture, move |s, v, _| s.set_material(index, true, |m| m.max_texture = v), 1.0).w_full()));
    }
    if !info.maps.is_empty() {
        let lines = info.maps.iter().map(|(slot, image)| {
            let size = doc.image_size(*image).map_or_else(|| "kept as is".to_owned(), |(w, h)| format!("{w}×{h}"));
            format!("{} {size}", slot.title())
        }).collect::<Vec<_>>().join("   ");
        body = body.child(text(lines).font(SMALL).color(T.faint).truncate().w_full().mt(2.0));
    }
    let edited = *change != MaterialEdit::default();
    body.child(row().w_full().justify_end().child(dim(outlined("Reset material", "Reset material", false, &REFRESH, move |s, _| s.reset_material(index)), edited)))
}

fn model_controls(s: &App, doc: &ModelDoc) -> El {
    let edit = s.model_edit();
    let mut list = col().w_full().gap(MATERIAL_ROW_GAP).transition(rune::Transition::opacity());
    let longest = (0..doc.materials.len()).filter_map(|i| doc.longest_texture(i)).max();
    if let Some(longest) = longest {
        let sizes = ladder(longest);
        if !sizes.is_empty() {
            let items = std::iter::once(plain("Full", None)).chain(sizes.into_iter().map(|n| plain(n.to_string(), Some(n)))).collect();
            // Selected only when every material with textures agrees.
            let all: Vec<Option<u32>> = (0..doc.materials.len()).filter(|i| doc.longest_texture(*i).is_some()).map(|i| edit.materials.get(&i).and_then(|m| m.max_texture)).collect();
            let agree = all.windows(2).all(|p| p[0] == p[1]);
            let current = if agree { all.first().copied().flatten() } else { Some(0) };
            list = list.child(labelled("All textures", segments_inset("all-textures", items, current, |s, v, _| s.set_all_texture_sizes(v), 1.0).w_full()).mb(MATERIAL_ROW_GAP));
        }
    }
    for (index, info) in doc.materials.iter().enumerate() {
        let change = edit.materials.get(&index).copied().unwrap_or_default();
        let open = s.edit.expanded == Some(index);
        let shade = doc.swatch(index, &edit);
        let tone = |c: f32| (c.clamp(0.0, 1.0) * 255.0) as u8;
        let swatch = color(&format!("#{:02X}{:02X}{:02X}", tone(shade[0]), tone(shade[1]), tone(shade[2])));
        let edited = change != MaterialEdit::default();
        let head = row().key(format!("material-{index}")).role("button").focusable(true).label(info.name.clone()).w_full().h(30.0).px(8.0).gap(8.0).items_center().radius(6.0)
            .hover(T.hover).press(T.press).cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.toggle_material(index, tx))
            .child(el().size(14.0, 14.0).radius(4.0).fill(swatch).border(1.0, T.line))
            .child(el().grow(1.0).min_w(0.0).child(text(info.name.clone()).font(LABEL).color(if edited { T.text } else { T.muted }).truncate()))
            .child(label(material_summary(doc, index, &change), SMALL, T.faint));
        // Drawn while open or still closing; otherwise its zero height hides nothing.
        let folding = s.edit.folding == Some(index) && s.edit.settling();
        let mut fold = el().key(format!("fold-{index}")).w_full().h(if open { material_body_height(doc, index, info) } else { 0.0 }).clip_soft(0.0).opacity(if open { 1.0 } else { 0.0 });
        if !open { fold = fold.inert(); }
        if open || folding { fold = fold.child(material_body(doc, index, info, &change)); }
        list = list.child(col().key(format!("mat-{index}")).w_full().radius(7.0).border(1.0, if open { T.line } else { Color::TRANSPARENT }).child(head).child(fold));
    }
    if !doc.keeps_scene {
        list = list.child(text("Saves as one static GLB: the node tree and animation aren't kept.").font(SMALL).color(T.faint).wrap().w_full().mt(4.0));
    }
    list
}

/// How tall the drawer is for `kind`. Fixed, so nothing that happens inside it can move the
/// preview: a model's material list scrolls within it.
fn panel_height(s: &App, kind: Kind) -> f32 {
    match kind {
        Kind::Texture => IMAGE_PANEL_H,
        Kind::Audio => AUDIO_PANEL_H,
        Kind::Model => (s.window_height * 0.3).clamp(210.0, 300.0),
    }
}

fn panel(s: &App, asset: &Asset, height: f32) -> El {
    let decoded = s.session_for(asset.key);
    let inner = match asset.kind {
        Kind::Audio => audio_controls(s),
        Kind::Texture => dim(image_controls(s), decoded.is_some()),
        Kind::Model => match decoded.as_deref() {
            Some(Decoded::Model(doc)) => model_controls(s, doc),
            _ => el().w_full().h_full().center().child(label(if s.edit.failure.is_some() { "" } else { "Reading the file\u{2026}" }, SMALL, T.muted)),
        },
    };
    let content = col().key("edit-controls").w_full().h_full().padding(CARD_PAD);
    let content = if asset.kind == Kind::Model {
        content.scroll(ScrollSpec { stick_bottom: false }).child(inner).scroll_bar_styled(scroll_style())
    } else {
        content.child(inner)
    };
    col().key("edit-panel").w_full().h(height).radius(WELL_RADIUS + 1.0).border(1.0, T.line).clip_rounded(WELL_RADIUS + 1.0).child(content)
}

/// The drawer under the preview. Always mounted, so opening it is a height changing and not an
/// element appearing, and the preview above gives up its room smoothly.
pub fn slot(s: &App, asset: &Asset) -> El {
    let height = panel_height(s, asset.kind);
    let open = s.edit.open;
    let mut drawer = el().key("edit-slot").w_full().h(if open { height + SLOT_GAP } else { 0.0 }).clip_soft(0.0).opacity(if open { 1.0 } else { 0.0 });
    if !open { drawer = drawer.inert(); }
    if open || s.edit.settling() { drawer = drawer.child(col().w_full().pt(SLOT_GAP).child(panel(s, asset, height))); }
    drawer
}

/// Source and output sizes as bars, and the compare and reset controls. Every row is always
/// here, whatever state the edit is in, so its layout never changes.
pub fn foot(s: &App, asset: &Asset) -> El {
    let edited = s.edit.is_edited(asset.key);
    let output = s.edit.output.as_ref().filter(|_| edited);
    let working = (edited && s.edit.is_rendering()) || s.edit.is_opening();
    let amount = output.map_or(asset.size, |o| o.bytes);
    let longest = asset.size.max(amount).max(1) as f32;
    let bar = |name: String, value: String, share: f32, fill: Color, key: &str| row().w_full().gap(8.0).items_center()
        .child(el().w(NAME_W).child(text(name).font(SMALL).color(T.muted).truncate()))
        .child(el().grow(1.0).min_w(0.0).h(5.0).radius(2.5).fill(T.line).child(el().key(key.to_owned()).w_frac(share.max(0.01)).h_full().radius(2.5).fill(fill)))
        .child(row().w(62.0).justify_end().child(label(value, LABEL, T.text)));
    let format_name = if edited {
        match asset.kind { Kind::Audio => "OGG", Kind::Texture => "PNG", Kind::Model => "GLB" }.to_owned()
    } else {
        asset.ext.to_uppercase()
    };
    let shown = if edited && output.is_none() { "\u{2014}".to_owned() } else { bytes(amount) };
    let note = if let Some(reason) = &s.edit.failure {
        format!("Can't edit this: {reason}")
    } else if s.edit.is_opening() {
        "Reading the file\u{2026}".to_owned()
    } else if working {
        "Rendering\u{2026}".to_owned()
    } else if !edited {
        "Unchanged, drags out the original".to_owned()
    } else {
        match (amount as f64 / asset.size.max(1) as f64 - 1.0) * 100.0 {
            c if c.abs() < 0.5 => "About the same size".to_owned(),
            c if c <= -99.5 => "Over 99% smaller".to_owned(),
            c if c < 0.0 => format!("{:.0}% smaller", -c),
            c => format!("{c:.0}% bigger"),
        }
    };
    let output_bar = bar(format!("{format_name} output"), shown, amount as f32 / longest, T.accent, "output-bar").opacity(if working { 0.45 } else { 1.0 }).motion(motion::SNAPPY);
    col().key("edit-foot").w_full().h_full().padding(CARD_PAD).gap(6.0).justify_between().radius(WELL_RADIUS + 1.0).border(1.0, T.line)
        .child(col().w_full().gap(4.0)
            .child(bar("Source".to_owned(), bytes(asset.size), asset.size as f32 / longest, T.faint, "source-bar"))
            .child(output_bar)
            .child(row().w_full().justify_end().child(text(note).font(SMALL).color(T.muted).truncate())))
        .child(dim(row().w_full().items_center().justify_between()
            .child(segments("compare", vec![plain("Original", true), plain("Edited", false)], s.edit.comparing, |s, v, _| s.set_comparing(v)))
            .child(outlined("Reset", "Reset", false, &REFRESH, |s, _| s.reset_edit())), edited))
}

/// The waveform with its section shaded and draggable.
pub fn markers(s: &App, wave: El) -> El {
    let Some((start, end, frames)) = s.audio_section() else { return wave };
    if frames == 0 { return wave }
    let width = s.stage_size[0];
    let px = |frame: u64| frame as f32 / frames as f32 * width;
    let (left, right) = (px(start), px(end));
    let shade = T.well.alpha(0.78);
    let mut wave = wave
        .child(el().key("shade-start").pin(Some(0.0), Some(0.0), None, Some(0.0)).w(left).fill(shade).inert())
        .child(el().key("shade-end").pin(Some(right), Some(0.0), Some(0.0), Some(0.0)).fill(shade).inert());
    if s.edit.open {
        let handle = |key: &str, name: &str, at: f32, marker: Marker| {
            el().key(key.to_owned()).role("slider").label(name.to_owned()).absolute(at - 7.0, 0.0).w(14.0).h_full().center().cursor(Cursor::ResizeColumn)
                .on_drag(move |s: &mut App, drag, tx| {
                    s.drag_marker(marker, drag);
                    // A marker stays under the pointer, rather than springing after it.
                    tx.instant();
                })
                .child(el().w(2.0).h_full().fill(T.accent).child(el().size(10.0, 12.0).radius(3.0).fill(T.accent).absolute(-4.0, 0.0)))
        };
        wave = wave.child(handle("mark-start", "Section start", left, Marker::Start)).child(handle("mark-end", "Section end", right, Marker::End));
    }
    wave
}
