//! Browse a folder of game assets: search them, preview them, drag copies out.
//! `cargo run --release` opens the window; `-- --snap out.png --scene model` renders headless.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[cfg(test)]
mod app_tests;
mod audio;
mod edit;
mod edit_ui;
mod editor;
mod export;
mod library;
mod model;
mod native_drag;
mod raster;
mod settings;
mod query;
mod stage;
mod texture_guess;
mod thumbs;
mod ui;
mod wave;

use rune::{App, AppSpec};

fn scene(name: &'static str) -> impl Fn(&mut app::App) + Copy {
    move |s| s.apply_scene(name)
}

macro_rules! scenes {
    ($($name:literal),* $(,)?) => {
        &[$(($name, |s: &mut app::App| scene($name)(s))),*]
    };
}

fn main() {
    let _ = rune::set_app_icon(include_bytes!("../assets/icon.png").as_slice());
    let spec = AppSpec::<app::App> {
        setup: Some(app::setup),
        poll: Some(|s, _, tx| s.poll(tx)),
        on_key: Some(ui::on_key),
        min_size: Some((1200.0, 600.0)),
        scenes: scenes!["textures", "models", "audio", "search", "search-help", "search-values", "small", "texture", "tile", "tile-seams", "big-texture", "model", "sound", "texture-sized", "settings", "settings-over-model", "edit-trim", "edit-zoom", "edit-loop", "edit-level", "edit-compress", "edit-texture", "edit-model", "edit-whole-model", "light-edit-model", "light-start", "light-model", "light-sound", "light-big-texture", "light-settings"],
        ..AppSpec::new("Asset Browser", (1520.0, 900.0), app::init, ui::view)
    };
    App::new(spec)
        .titlebar(rune::Titlebar::new(ui::TITLEBAR_H))
        .compositor(stage::StageCompositor::default())
        .on_window(remember_window)
        .run();
}

#[cfg(windows)]
fn remember_window(window: &rune::WindowHandle) {
    use winit_raw::RawWindowHandle;
    let _ = window.with_raw_handle(|raw| {
        if let RawWindowHandle::Win32(h) = raw.as_raw() { native_drag::set_window(h.hwnd.get()); }
    });
}

#[cfg(not(windows))]
fn remember_window(_: &rune::WindowHandle) {}
