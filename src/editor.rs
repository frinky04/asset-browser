//! The editing state behind the inspector's Edit panel: one recipe per asset, and for the selected
//! asset a decoded copy that recipes are rendered against. Rendering always goes through
//! `edit::render` and lands in the staged-copy cache, so the preview is the file a drag hands over.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rune::{Latest, Pixels, Spring, TextEdit, Tx, Waker};

use crate::app::{App, Preview, WAVE_H, WAVE_W};
use crate::edit::mesh::{Origin, SurfaceMaterial, TextureFilter};
use crate::edit::{self, AudioEdit, Decoded, Edit, ImageEdit, MaterialEdit, ModelDoc, ModelEdit, Pcm, Rendered, Shown, ball, sound};
use crate::export::{self, Options};
use crate::library::{Asset, Kind};
use crate::model::Image;
use crate::stage::Scene;

/// Sliders move several times a second; renders wait for a brief pause.
const RENDER_PAUSE: Duration = Duration::from_millis(140);
pub const SEAM_W: u32 = 640;
pub const SEAM_H: u32 = 120;
const SEAM_MS_DEFAULT: f32 = 30.0;
/// Shortest section that can be picked, in milliseconds.
const MIN_SECTION_MS: u64 = 10;
/// Long enough for the slowest animation setting to finish.
const SETTLE_FOR: Duration = Duration::from_millis(900);
/// How the panel and its rows open and close.
pub const PANEL_SPRING: Spring = Spring::new(0.3, 0.0);
/// The fewest frames the timeline can be zoomed to show.
const MIN_VIEW_FRAMES: f64 = 64.0;
const TIMELINE_ZOOM_SPEED: f32 = 0.006;
/// Points the pointer travels before a press on the waveform becomes a drag.
const REGION_TRAVEL: f32 = 3.0;
/// How far before a loop's end "hear the seam" starts.
const SEAM_LEAD_SECONDS: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioTab {
    Trim,
    Loop,
    Level,
    Compress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelTab {
    Materials,
    WholeModel,
}

/// Which end of the audio section a control moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Start,
    End,
}

struct Session {
    key: u64,
    decoded: Arc<Decoded>,
}

/// The finished output of the current recipe.
pub struct Output {
    pub name: String,
    pub bytes: u64,
}

struct Finished {
    name: String,
    bytes: u64,
    shown: Shown,
}

/// How a clip that's playing lines up with the source's own timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    start: f32,
    length: f32,
    looped: bool,
}

impl Timeline {
    /// The place in the source that `position` seconds of playback has reached.
    fn source_time(self, position: f32) -> f32 {
        let into = if self.looped && self.length > 0.0 { position % self.length } else { position.min(self.length) };
        self.start + into
    }
}

pub struct Editor {
    pub open: bool,
    /// Only recipes that change something are kept here.
    recipes: HashMap<u64, Edit>,
    session: Option<Session>,
    opening: Latest<(u64, Result<(Arc<Decoded>, Option<Arc<sound::PeakIndex>>), String>)>,
    rendering: Latest<(u64, Result<Finished, String>)>,
    cutting: Latest<(u64, Arc<Pcm>, f32)>,
    tuning: Latest<(u64, u64)>,
    pub failure: Option<String>,
    pub output: Option<Output>,
    /// The preview is showing the unedited asset.
    pub comparing: bool,
    pub audio_tab: AudioTab,
    pub model_tab: ModelTab,
    /// The scale field's text, which may be half typed.
    pub scale_text: TextEdit,
    /// The edited section's loudest moment, in decibels of full scale.
    pub peak_db: Option<f32>,
    peak_index: Option<Arc<sound::PeakIndex>>,
    /// The frames the timeline shows, when zoomed in; `None` shows the whole sound.
    pub view: Option<(u64, u64)>,
    /// The timeline drawn for `view`.
    pub timeline_wave: Pixels,
    /// Where a drag across the waveform began, once it has moved far enough to be one.
    region_anchor: u64,
    region_active: bool,
    playhead_was_in_view: bool,
    /// The material whose controls are unfolded.
    pub expanded: Option<usize>,
    pub seam: Pixels,
    seam_ms: f32,
    seam_drag_end: u64,
    /// The section, with the loop's seam blended, before it's compressed.
    clean: Option<Arc<Pcm>>,
    /// What the compressed file decodes to.
    coded: Option<Arc<Pcm>>,
    rendered_image: Option<Arc<Image>>,
    pub timeline: Option<Timeline>,
    autoplay_waiting: bool,
    /// Until when the panel or a material row is still opening or closing.
    settling_until: Option<Instant>,
    /// The material row that just folded away, kept drawn while it closes.
    pub folding: Option<usize>,
    /// The textures the last render made, which the stage shows unless comparing.
    model_images: Option<Vec<Arc<Image>>>,
    /// The material hovered in the list.
    focus: Option<usize>,
    /// A rendered sphere per document material.
    pub balls: Vec<Pixels>,
    /// A small copy of each document image.
    pub map_thumbs: Vec<Pixels>,
    ball_seen: Vec<Option<BallSig>>,
    thumb_seen: Vec<usize>,
    drawing: Latest<Drawn>,
    shrunk: Arc<ball::Shrunk>,
}

/// Everything a material ball's picture depends on.
#[derive(Clone, Copy, PartialEq)]
struct BallSig {
    material: SurfaceMaterial,
    maps: [usize; 4],
}

struct Drawn {
    key: u64,
    balls: Vec<(usize, BallSig, Vec<u8>)>,
    thumbs: Vec<(usize, usize, Arc<Image>)>,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            open: false,
            recipes: HashMap::new(),
            session: None,
            opening: Latest::default(),
            rendering: Latest::default().debounce(RENDER_PAUSE),
            cutting: Latest::default().debounce(RENDER_PAUSE),
            tuning: Latest::default(),
            failure: None,
            output: None,
            comparing: false,
            audio_tab: AudioTab::Trim,
            model_tab: ModelTab::Materials,
            scale_text: TextEdit::new("1"),
            peak_db: None,
            peak_index: None,
            view: None,
            timeline_wave: Pixels::new(),
            region_anchor: 0,
            region_active: false,
            playhead_was_in_view: false,
            expanded: None,
            seam: Pixels::new(),
            seam_ms: SEAM_MS_DEFAULT,
            seam_drag_end: 0,
            clean: None,
            coded: None,
            rendered_image: None,
            timeline: None,
            autoplay_waiting: false,
            settling_until: None,
            folding: None,
            model_images: None,
            focus: None,
            balls: Vec::new(),
            map_thumbs: Vec::new(),
            ball_seen: Vec::new(),
            thumb_seen: Vec::new(),
            drawing: Latest::default(),
            shrunk: Arc::default(),
        }
    }
}

impl Editor {
    pub fn set_waker(&self, waker: &Waker) {
        self.opening.set_waker(waker.clone());
        self.rendering.set_waker(waker.clone());
        self.cutting.set_waker(waker.clone());
        self.tuning.set_waker(waker.clone());
        self.drawing.set_waker(waker.clone());
    }

    pub fn busy(&self) -> bool {
        self.opening.is_pending() || self.rendering.is_pending() || self.cutting.is_pending() || self.tuning.is_pending() || self.drawing.is_pending()
    }

    pub fn is_rendering(&self) -> bool {
        self.rendering.is_pending()
    }

    /// Whether something the panel opened or folded is still moving.
    pub fn settling(&self) -> bool {
        self.settling_until.is_some_and(|t| Instant::now() < t)
    }

    fn start_settling(&mut self) {
        self.settling_until = Some(Instant::now() + SETTLE_FOR);
    }

    pub fn is_tuning(&self) -> bool {
        self.tuning.is_pending()
    }

    pub fn is_opening(&self) -> bool {
        self.opening.is_pending()
    }

    fn decoded(&self, key: u64) -> Option<&Arc<Decoded>> {
        self.session.as_ref().filter(|s| s.key == key).map(|s| &s.decoded)
    }

    pub fn is_edited(&self, key: u64) -> bool {
        self.recipes.contains_key(&key)
    }

    /// What a drag of the asset `key` should apply.
    pub fn recipe(&self, key: u64) -> Option<Arc<Edit>> {
        self.recipes.get(&key).map(|e| Arc::new(e.clone()))
    }

    /// The values material `index`'s ball was last drawn with.
    #[cfg(test)]
    pub fn drawn_ball(&self, index: usize) -> Option<SurfaceMaterial> {
        self.ball_seen.get(index).copied().flatten().map(|sig| sig.material)
    }

    #[cfg(test)]
    pub fn output_bytes(&self) -> Option<u64> {
        self.output.as_ref().map(|o| o.bytes)
    }
}

impl App {
    pub(crate) fn selected_asset(&self) -> Option<Asset> {
        self.selected.and_then(|i| self.lib.assets.get(i as usize)).cloned()
    }

    fn selected_key(&self) -> Option<u64> {
        self.selected_asset().map(|a| a.key)
    }

    /// The selected asset's decoded source, once it's been read.
    fn session_source(&self) -> Option<Arc<Decoded>> {
        self.edit.decoded(self.selected_key()?).cloned()
    }

    fn audio_source(&self) -> Option<Arc<Pcm>> {
        match self.session_source()?.as_ref() {
            Decoded::Audio(pcm) => Some(pcm.clone()),
            _ => None,
        }
    }

    /// The source's sample data and the section picked within it, for the waveform's markers.
    pub fn audio_section(&self) -> Option<(u64, u64, u64)> {
        let pcm = self.audio_source()?;
        let (start, end) = self.audio_edit().bounds(pcm.frames());
        Some((start, end, pcm.frames()))
    }

    /// The decoded source of asset `key`, if that's what's being edited.
    pub fn session_for(&self, key: u64) -> Option<Arc<Decoded>> {
        self.edit.decoded(key).cloned()
    }

    pub fn source_format(&self) -> Option<(u32, u16)> {
        self.audio_source().map(|p| (p.rate, p.channels))
    }

    pub fn model_doc(&self) -> Option<Arc<Decoded>> {
        self.session_source().filter(|d| matches!(d.as_ref(), Decoded::Model(_)))
    }

    pub fn audio_edit(&self) -> AudioEdit {
        match self.selected_key().and_then(|k| self.edit.recipes.get(&k)) {
            Some(Edit::Audio(e)) => *e,
            _ => AudioEdit::default(),
        }
    }

    pub fn image_edit(&self) -> ImageEdit {
        match self.selected_key().and_then(|k| self.edit.recipes.get(&k)) {
            Some(Edit::Image(e)) => *e,
            _ => ImageEdit::default(),
        }
    }

    pub fn model_edit(&self) -> ModelEdit {
        match self.selected_key().and_then(|k| self.edit.recipes.get(&k)) {
            Some(Edit::Model(e)) => e.clone(),
            _ => ModelEdit::default(),
        }
    }

    /// Stores what the selected asset should become.
    fn store_recipe(&mut self, edit: Edit) {
        let Some(key) = self.selected_key() else { return };
        let was_edited = self.edit.is_edited(key);
        if edit.is_unchanged() {
            self.edit.recipes.remove(&key);
        } else {
            self.edit.recipes.insert(key, edit);
        }
        // Tiles show which assets carry an edit.
        if was_edited != self.edit.is_edited(key) { self.generation += 1; }
    }

    /// Gives the selected asset a recipe before its source has been read, for snapshots.
    pub(crate) fn seed_scene_edit(&mut self, edit: Edit) {
        self.store_recipe(edit);
        self.ensure_session();
    }

    /// Unfolds a material's controls, or folds them if they're showing.
    pub fn toggle_material(&mut self, index: usize, tx: &mut Tx) {
        self.edit.folding = self.edit.expanded;
        self.edit.expanded = if self.edit.expanded == Some(index) { None } else { Some(index) };
        self.edit.start_settling();
        tx.motion(PANEL_SPRING);
    }

    pub fn toggle_editor(&mut self, tx: &mut Tx) {
        self.edit.open = !self.edit.open;
        self.edit.start_settling();
        tx.motion(PANEL_SPRING);
        self.edit.focus = None;
        // Zoom is an editing tool, and browsing always shows the whole sound.
        if !self.edit.open { self.edit.view = None; }
        if self.edit.open {
            self.ensure_session();
            self.update_surface();
        } else if self.selected_key().is_some_and(|k| !self.edit.is_edited(k)) {
            self.clear_surface();
        }
    }

    pub fn reset_edit(&mut self) {
        let Some(asset) = self.selected_asset() else { return };
        if self.edit.recipes.remove(&asset.key).is_some() { self.generation += 1; }
        self.edit.comparing = false;
        self.sync_scale_text();
        self.edit_changed();
    }

    /// Called when the selection changes: drops the old source, and reads the new one if it
    /// will be needed.
    pub(crate) fn editor_on_select(&mut self) {
        self.edit.session = None;
        self.edit.output = None;
        self.edit.clean = None;
        self.edit.coded = None;
        self.edit.rendered_image = None;
        self.edit.timeline = None;
        self.edit.failure = None;
        self.edit.comparing = false;
        self.edit.expanded = None;
        self.edit.folding = None;
        self.edit.peak_index = None;
        self.edit.view = None;
        self.edit.peak_db = None;
        self.edit.model_images = None;
        self.edit.focus = None;
        self.edit.balls.clear();
        self.edit.map_thumbs.clear();
        self.edit.ball_seen.clear();
        self.edit.thumb_seen.clear();
        self.edit.drawing.cancel();
        self.edit.shrunk = Arc::default();
        self.edit.autoplay_waiting = false;
        self.clear_surface();
        self.ensure_session();
    }

    fn clear_surface(&mut self) {
        if self.stage.surface.take().is_some() { self.stage.surface_revision += 1; }
    }

    /// Whether audio selected with autoplay on should wait for its edit before starting.
    pub(crate) fn defer_autoplay(&mut self) -> bool {
        let edited = self.selected_key().is_some_and(|k| self.edit.is_edited(k));
        self.edit.autoplay_waiting = edited;
        edited
    }

    fn ensure_session(&mut self) {
        let Some(asset) = self.selected_asset() else { return };
        if !self.edit.open && !self.edit.is_edited(asset.key) { return }
        if self.edit.decoded(asset.key).is_some() { return }
        self.edit.failure = None;
        let path = self.lib.path(&asset);
        let library = self.lib.root.clone();
        self.edit.opening.request((asset, path), move |(asset, path)| {
            let opened = edit::open(&path, asset.kind, &library).map(|decoded| {
                let peaks = match &decoded {
                    Decoded::Audio(pcm) => Some(Arc::new(sound::PeakIndex::build(pcm))),
                    _ => None,
                };
                (Arc::new(decoded), peaks)
            });
            (asset.key, opened)
        });
    }

    /// The size the render should use, matching what the inspector's drag bar asks for.
    fn render_options(&self, asset: &Asset) -> Options {
        let max_size = match self.edit.decoded(asset.key).map(Arc::as_ref) {
            Some(Decoded::Image(image)) => self.texture_size.filter(|m| *m < image.width().max(image.height())),
            _ => None,
        };
        Options { max_size, nearest: self.nearest, companions: self.companions, edit: self.edit.recipe(asset.key) }
    }

    /// Brings previews and outputs in line with the current recipe, and starts whatever render
    /// that takes.
    pub(crate) fn edit_changed(&mut self) {
        let Some(asset) = self.selected_asset() else { return };
        self.redraw_seam();
        let Some(decoded) = self.edit.decoded(asset.key).cloned() else { return };
        // The last size stays up while the next one renders, so the readout doesn't blink.
        self.edit.coded = None;
        let options = self.render_options(&asset);
        self.update_surface();
        if options.edit.is_none() {
            self.edit.output = None;
            self.edit.peak_db = None;
            self.edit.rendered_image = None;
            self.edit.model_images = None;
            self.update_surface();
            self.edit.clean = None;
            self.edit.rendering.cancel();
            self.edit.cutting.cancel();
            if asset.kind == Kind::Texture { self.show_plain_size(); }
            self.refresh_audition();
            return;
        }
        if let (Decoded::Audio(source), Some(Edit::Audio(edit))) = (decoded.as_ref(), options.edit.as_deref()) {
            let (source, edit) = (source.clone(), *edit);
            self.edit.cutting.request((asset.key, source, edit), |(key, source, edit)| {
                let clean = sound::cut(&source, &edit);
                let peak = sound::peak_db(&clean);
                (key, Arc::new(clean), peak)
            });
        }
        self.edit.rendering.request((asset, options, decoded), |(asset, options, decoded)| {
            let finished = edit::render(&decoded, asset.name(), &options).map(|Rendered { name, bytes, shown }| {
                // Failing to cache is fine: the drag will just make the file itself.
                let _ = export::stage_rendered(&asset, &options, &name, &bytes);
                Finished { bytes: bytes.len() as u64, name, shown }
            });
            (asset.key, finished)
        });
    }

    /// A full poll, reporting whether editing work landed in it.
    #[cfg(test)]
    pub(crate) fn poll_editor_for_test(&mut self, tx: &mut Tx) -> bool {
        let landed = self.poll_editor();
        self.poll(tx);
        landed
    }

    /// Lands whatever background editing work has finished; true if anything did.
    pub(crate) fn poll_editor(&mut self) -> bool {
        let mut changed = false;
        if let Some((key, result)) = self.edit.opening.take() {
            changed = true;
            if self.selected_key() == Some(key) {
                match result {
                    Ok((decoded, peaks)) => {
                        self.edit.peak_index = peaks;
                        if let Decoded::Model(doc) = decoded.as_ref() {
                            self.edit.balls = doc.materials.iter().map(|_| Pixels::new()).collect();
                            self.edit.ball_seen = vec![None; doc.materials.len()];
                            self.edit.map_thumbs = (0..doc.image_count()).map(|_| Pixels::new()).collect();
                            self.edit.thumb_seen = vec![0; doc.image_count()];
                            self.sync_scale_text();
                        }
                        self.edit.session = Some(Session { key, decoded });
                        self.redraw_timeline();
                        self.edit_changed();
                    }
                    Err(reason) => self.edit.failure = Some(reason),
                }
            }
        }
        if let Some((key, clean, peak)) = self.edit.cutting.take() {
            changed = true;
            if self.selected_key() == Some(key) {
                self.edit.clean = Some(clean);
                self.edit.peak_db = Some(peak);
                self.refresh_audition();
            }
        }
        if let Some((key, result)) = self.edit.rendering.take() {
            changed = true;
            if self.selected_key() == Some(key) { self.land_render(result); }
        }
        if let Some(drawn) = self.edit.drawing.take() {
            changed = true;
            self.land_drawn(drawn);
        }
        if let Some((key, end)) = self.edit.tuning.take() {
            changed = true;
            if self.selected_key() == Some(key) {
                let start = self.audio_section().map_or(0, |s| s.0);
                self.set_section(start, end, true);
            }
        }
        changed
    }

    fn land_render(&mut self, result: Result<Finished, String>) {
        let finished = match result {
            Ok(finished) => finished,
            Err(reason) => {
                self.edit.failure = Some(reason);
                return;
            }
        };
        self.edit.failure = None;
        self.edit.output = Some(Output { name: finished.name, bytes: finished.bytes });
        match finished.shown {
            Shown::Image(image) => {
                self.edit.rendered_image = Some(image);
                self.show_rendered_image();
            }
            Shown::Audio(pcm) => {
                self.edit.coded = Some(pcm);
                self.refresh_audition();
            }
            Shown::Model(images) => self.update_surface_with(Some(images)),
        }
    }

    /// Puts the current render on the stage. A no-op while comparing against the original.
    pub(crate) fn show_rendered_image(&mut self) {
        if self.edit.comparing { return }
        let Some(image) = self.edit.rendered_image.clone() else { return };
        self.show_texture(image);
    }

    /// Whether the selected texture carries an image edit.
    pub(crate) fn has_image_edit(&self) -> bool {
        self.selected_key().is_some_and(|k| matches!(self.edit.recipes.get(&k), Some(Edit::Image(_))))
    }

    pub fn set_comparing(&mut self, comparing: bool) {
        self.edit.comparing = comparing;
        if matches!(self.preview, Preview::Texture(_)) {
            if comparing { self.show_plain_size(); } else { self.show_rendered_image(); }
        }
        self.update_surface();
        self.refresh_audition();
    }

    pub fn set_image_edit(&mut self, edit: ImageEdit) {
        self.store_recipe(Edit::Image(edit));
        self.edit_changed();
    }

    // --- Models ---

    fn update_surface(&mut self) {
        self.update_surface_with(None);
    }

    /// Recomputes the preview's materials, optionally with a new set of rendered textures.
    fn update_surface_with(&mut self, rendered: Option<Vec<Arc<Image>>>) {
        let Some(decoded) = self.model_doc() else { return };
        let Decoded::Model(doc) = decoded.as_ref() else { return };
        // Kept apart from the stage's copy, which holds the originals while comparing.
        if rendered.is_some() { self.edit.model_images = rendered; }
        let (edit, images) = if self.edit.comparing {
            (ModelEdit::default(), doc.original_images())
        } else {
            (self.model_edit(), self.edit.model_images.clone().unwrap_or_else(|| doc.original_images()))
        };
        self.request_balls(doc, &edit, &images);
        let mut surface = doc.surface(&edit, images);
        surface.focus = self.edit.focus;
        surface.origin = (self.edit.model_tab == ModelTab::WholeModel).then(|| doc.pivot(edit.origin));
        self.stage.surface = Some(surface);
        self.stage.surface_revision += 1;
    }

    /// Picks out one material on the stage, for as long as it's hovered in the list.
    pub fn set_material_focus(&mut self, index: usize, inside: bool) {
        let focus = if inside { Some(index) } else { self.edit.focus.filter(|f| *f != index) };
        self.edit.focus = focus;
        if let Some(surface) = &mut self.stage.surface {
            surface.focus = focus;
            self.stage.surface_revision += 1;
        }
    }

    /// Redraws the material balls and map thumbnails that are out of date, off the UI thread.
    fn request_balls(&mut self, doc: &ModelDoc, edit: &ModelEdit, images: &[Arc<Image>]) {
        let Some(key) = self.selected_key() else { return };
        let address = |image: Option<u32>| image.and_then(|i| images.get(i as usize)).map_or(0, |a| Arc::as_ptr(a) as usize);
        let mut balls = Vec::new();
        for index in 0..doc.materials.len() {
            let material = doc.material_surface(index, edit);
            let sig = BallSig { material, maps: [address(material.image), address(material.surface_image), address(material.normal_image), address(material.emissive_image)] };
            if self.edit.ball_seen.get(index).copied().flatten() != Some(sig) { balls.push((index, sig)); }
        }
        let used: std::collections::BTreeSet<usize> = doc.materials.iter().flat_map(|m| m.maps.iter().map(|(_, i)| *i)).collect();
        let thumbs: Vec<(usize, Arc<Image>)> = used.into_iter().filter_map(|i| {
            let image = images.get(i)?;
            (self.edit.thumb_seen.get(i).copied() != Some(Arc::as_ptr(image) as usize)).then(|| (i, image.clone()))
        }).collect();
        if balls.is_empty() && thumbs.is_empty() {
            self.edit.drawing.cancel();
            return;
        }
        let images = images.to_vec();
        let shrunk = self.edit.shrunk.clone();
        self.edit.drawing.request((key, balls, thumbs, images), move |(key, balls, thumbs, images)| {
            let small = |index: Option<u32>| index.and_then(|i| images.get(i as usize)).map(|image| shrunk.get(image));
            let balls = balls.into_iter().map(|(index, sig): (usize, BallSig)| {
                let m = sig.material;
                let maps = ball::Maps { base: small(m.image), surface: small(m.surface_image), normal: small(m.normal_image), emissive: small(m.emissive_image) };
                (index, sig, ball::draw(&m, &maps, ball::BALL_PX))
            }).collect();
            let thumbs = thumbs.into_iter().map(|(index, image)| {
                let shown = shrunk.get(&image);
                (index, Arc::as_ptr(&image) as usize, shown)
            }).collect();
            Drawn { key, balls, thumbs }
        });
    }

    fn land_drawn(&mut self, drawn: Drawn) {
        if self.selected_key() != Some(drawn.key) { return }
        for (index, sig, rgba) in drawn.balls {
            if let Some(pixels) = self.edit.balls.get(index) { pixels.set(ball::BALL_PX, ball::BALL_PX, rgba); }
            if let Some(slot) = self.edit.ball_seen.get_mut(index) { *slot = Some(sig); }
        }
        for (index, address, shown) in drawn.thumbs {
            if let Some(pixels) = self.edit.map_thumbs.get(index) { pixels.set(shown.width, shown.height, shown.rgba.clone()); }
            if let Some(slot) = self.edit.thumb_seen.get_mut(index) { *slot = address; }
        }
    }

    /// Sets the filter for the whole model, clearing any set on single textures, so the choice
    /// made last is the one that shows. They can be set again afterwards.
    pub fn set_model_filter(&mut self, filter: Option<TextureFilter>) {
        let mut edit = self.model_edit();
        edit.filter = filter;
        edit.textures.clear();
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    /// Sets one image's filter, or with `None` returns it to the model's.
    pub fn set_texture_filter(&mut self, image: usize, filter: Option<TextureFilter>) {
        let mut edit = self.model_edit();
        match filter {
            Some(filter) => edit.textures.insert(image, filter),
            None => edit.textures.remove(&image),
        };
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    pub fn set_model_tab(&mut self, tab: ModelTab) {
        self.edit.model_tab = tab;
        self.update_surface();
    }

    pub fn set_origin(&mut self, origin: Option<Origin>) {
        let mut edit = self.model_edit();
        edit.origin = origin;
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    fn store_scale(&mut self, scale: f32) {
        let mut edit = self.model_edit();
        edit.scale = ((scale - 1.0).abs() > 1e-6).then_some(scale.clamp(1e-4, 1e4));
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    /// Takes a scale from a preset, and shows it in the field.
    pub fn set_scale(&mut self, scale: f32) {
        self.store_scale(scale);
        self.sync_scale_text();
    }

    /// Reads the scale field. Half-typed text simply doesn't count yet.
    pub fn scale_typed(&mut self) {
        if let Ok(value) = self.edit.scale_text.text.trim().parse::<f32>() && value.is_finite() && value > 0.0 { self.store_scale(value); }
    }

    fn sync_scale_text(&mut self) {
        let shown = self.model_edit().scale.unwrap_or(1.0).to_string();
        self.edit.scale_text.replace_text(shown);
    }

    /// Converts one normal map between DirectX and OpenGL, or back.
    pub fn toggle_normal_flip(&mut self, image: usize) {
        let mut edit = self.model_edit();
        if !edit.flip_normal.remove(&image) { edit.flip_normal.insert(image); }
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    /// Sets the level. `commit` is false while the slider is still moving.
    pub fn set_gain(&mut self, gain_db: f32, commit: bool) {
        let mut edit = self.audio_edit();
        edit.gain_db = gain_db;
        self.store_recipe(Edit::Audio(edit));
        if commit { self.edit_changed(); }
    }

    /// Applies a look to a material in one step.
    pub fn apply_preset(&mut self, index: usize, roughness: f32, metallic: f32) {
        self.set_material(index, true, |m| {
            m.roughness = Some(roughness);
            m.metallic = Some(metallic);
        });
    }

    /// Changes one material. `commit` is false while a slider is still moving.
    pub fn set_material(&mut self, index: usize, commit: bool, change: impl FnOnce(&mut MaterialEdit)) {
        let mut edit = self.model_edit();
        let entry = edit.materials.entry(index).or_default();
        change(entry);
        if let Some(decoded) = self.model_doc()
            && let Decoded::Model(doc) = decoded.as_ref()
        {
            let info = &doc.materials[index];
            let close = |a: f32, b: f32| (a - b).abs() < 0.0005;
            if entry.roughness.is_some_and(|v| close(v, info.roughness)) { entry.roughness = None; }
            if entry.metallic.is_some_and(|v| close(v, info.metallic)) { entry.metallic = None; }
            if entry.normal.is_some_and(|v| close(v, info.normal_scale)) { entry.normal = None; }
            if entry.color.is_some_and(|c| (0..3).all(|i| close(c[i], info.color[i]))) { entry.color = None; }
        }
        if *entry == MaterialEdit::default() { edit.materials.remove(&index); }
        self.store_recipe(Edit::Model(edit));
        if commit { self.edit_changed(); } else { self.update_surface(); }
    }

    /// Sets the texture size on every material that has a texture bigger than it.
    pub fn set_all_texture_sizes(&mut self, size: Option<u32>) {
        let Some(decoded) = self.model_doc() else { return };
        let Decoded::Model(doc) = decoded.as_ref() else { return };
        let mut edit = self.model_edit();
        for index in 0..doc.materials.len() {
            let Some(longest) = doc.longest_texture(index) else { continue };
            let entry = edit.materials.entry(index).or_default();
            entry.max_texture = size.filter(|s| *s < longest);
            if *entry == MaterialEdit::default() { edit.materials.remove(&index); }
        }
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    pub fn reset_material(&mut self, index: usize) {
        let mut edit = self.model_edit();
        edit.materials.remove(&index);
        self.store_recipe(Edit::Model(edit));
        self.edit_changed();
    }

    // --- Audio ---

    pub fn set_audio_edit(&mut self, edit: AudioEdit) {
        self.store_recipe(Edit::Audio(edit));
        self.edit_changed();
    }

    /// Sets the Vorbis quality. `commit` is false while the slider is still moving.
    pub fn set_quality(&mut self, quality: u8, commit: bool) {
        let mut edit = self.audio_edit();
        edit.quality = quality;
        self.store_recipe(Edit::Audio(edit));
        if commit { self.edit_changed(); }
    }

    /// Moves the section. `commit` is false while a marker is still being dragged.
    pub fn set_section(&mut self, start: u64, end: u64, commit: bool) {
        let Some(pcm) = self.audio_source() else { return };
        let frames = pcm.frames();
        let shortest = (u64::from(pcm.rate) * MIN_SECTION_MS / 1000).max(1).min(frames);
        let end = end.clamp(shortest, frames);
        let start = start.min(end - shortest);
        let mut edit = self.audio_edit();
        edit.section = (start > 0 || end < frames).then_some((start, end));
        self.store_recipe(Edit::Audio(edit));
        if commit { self.edit_changed(); } else { self.redraw_seam(); }
    }

    /// The frames on the timeline, and the sound's length. The whole sound unless zoomed.
    pub fn audio_view(&self) -> Option<(u64, u64, u64)> {
        let pcm = self.audio_source()?;
        let frames = pcm.frames();
        let (start, end) = self.edit.view.unwrap_or((0, frames));
        Some((start, end, frames))
    }

    /// The frame `x` points along the timeline falls on.
    fn frame_at(&self, x: f32) -> Option<u64> {
        let (start, end, _) = self.audio_view()?;
        let along = f64::from(x / self.stage_size[0].max(1.0)).clamp(0.0, 1.0);
        Some(start + (along * (end - start) as f64) as u64)
    }

    /// Where `seconds` of the sound sits along the timeline, in points.
    pub(crate) fn timeline_x(&self, seconds: f32, duration: f32) -> f32 {
        let width = self.stage_size[0];
        match (self.edit.view, self.source_format()) {
            (Some((start, end)), Some((rate, _))) => {
                let along = (f64::from(seconds) * f64::from(rate) - start as f64) / (end - start) as f64;
                // Off the end it's clipped by the timeline's own edge.
                (along as f32 * width).clamp(-20.0, width + 20.0)
            }
            _ => seconds / duration.max(0.001) * width,
        }
    }

    fn set_view(&mut self, start: f64, span: f64) {
        let Some(pcm) = self.audio_source() else { return };
        let frames = pcm.frames() as f64;
        let span = span.clamp(MIN_VIEW_FRAMES.min(frames), frames);
        if span >= frames {
            self.edit.view = None;
            return;
        }
        let start = start.clamp(0.0, frames - span);
        self.edit.view = Some((start as u64, (start + span) as u64));
        self.redraw_timeline();
    }

    pub(crate) fn redraw_timeline(&mut self) {
        let Some((start, end)) = self.edit.view else { return };
        let (Some(pcm), Some(index)) = (self.audio_source(), self.edit.peak_index.clone()) else { return };
        let px = sound::draw_timeline(&pcm, &index, start, end, WAVE_W, WAVE_H, self.wave_color());
        self.edit.timeline_wave.set(WAVE_W, WAVE_H, px);
    }

    /// A wheel over the timeline: up and down zoom about the pointer, sideways pans.
    pub fn wheel_timeline(&mut self, wheel: rune::WheelEvent, tx: &mut Tx) -> bool {
        if !self.edit.open { return false }
        let Some((start, end, _)) = self.audio_view() else { return false };
        let span = (end - start) as f64;
        let per_point = span / f64::from(self.stage_size[0].max(1.0));
        let sideways = if wheel.mods.shift { wheel.dy } else if wheel.dx.abs() > wheel.dy.abs() { wheel.dx } else { 0.0 };
        if sideways != 0.0 {
            self.set_view(start as f64 - f64::from(sideways) * per_point, span);
        } else {
            let anchor = f64::from(wheel.x) * per_point;
            let next = span * f64::from(-wheel.dy * TIMELINE_ZOOM_SPEED).exp();
            self.set_view(start as f64 + anchor - anchor / span * next, next);
        }
        // The markers and the picture have to move together.
        tx.instant();
        true
    }

    pub fn fit_timeline(&mut self) {
        self.edit.view = None;
    }

    /// Dragging along the zoomed timeline's overview bar.
    pub fn drag_overview(&mut self, drag: &mut rune::Drag, tx: &mut Tx) {
        let Some((start, end, frames)) = self.audio_view() else { return };
        let span = (end - start) as f64;
        let at = f64::from(drag.x / drag.w.max(1.0)).clamp(0.0, 1.0) * frames as f64;
        if drag.phase == rune::Phase::Down {
            // Grabbing the thumb holds it by that spot; anywhere else centres it there.
            let on_thumb = (start as f64..end as f64).contains(&at);
            drag.data = if on_thumb { ((at - start as f64) / span) as f32 } else { 0.5 };
        }
        self.set_view(at - f64::from(drag.data) * span, span);
        tx.instant();
    }

    /// The waveform's own drag. In the editor it picks out a section and a plain click seeks;
    /// otherwise it scrubs.
    pub fn drag_waveform(&mut self, drag: &rune::Drag, tx: &mut Tx) {
        use rune::Phase;
        let editing = self.edit.open && self.audio_section().is_some();
        if !editing {
            if drag.phase != Phase::Up { self.seek_fraction(drag.x / drag.w.max(1.0), tx); }
            return;
        }
        let Some(frame) = self.frame_at(drag.x) else { return };
        match drag.phase {
            Phase::Down => {
                self.edit.region_anchor = frame;
                self.edit.region_active = false;
            }
            Phase::Move => {
                if !self.edit.region_active && drag.dx.abs() < REGION_TRAVEL { return }
                self.edit.region_active = true;
                let anchor = self.edit.region_anchor;
                self.set_section(anchor.min(frame), anchor.max(frame), false);
                tx.instant();
            }
            Phase::Up if self.edit.region_active => {
                self.edit.region_active = false;
                let anchor = self.edit.region_anchor;
                let (mut first, mut last) = (anchor.min(frame), anchor.max(frame));
                if self.audio_edit().looped && let Some(pcm) = self.audio_source() {
                    first = sound::snap_to_zero(&pcm, first);
                    last = sound::snap_to_zero(&pcm, last);
                }
                self.set_section(first, last, true);
                tx.instant();
            }
            Phase::Up => {
                let frames = self.audio_view().map_or(1, |(_, _, frames)| frames.max(1));
                self.seek_fraction((frame as f64 / frames as f64) as f32, tx);
            }
        }
    }

    /// Pages a zoomed timeline along when playback runs off its edge; true if it moved.
    pub(crate) fn follow_playhead(&mut self, seconds: f32) -> bool {
        let (Some((start, end)), Some((rate, _))) = (self.edit.view, self.source_format()) else { return false };
        let frame = (f64::from(seconds) * f64::from(rate)) as u64;
        let target = self.audio.is_playing().then(|| sound::follow_view((start, end), frame, self.edit.playhead_was_in_view)).flatten();
        if let Some(from) = target { self.set_view(from as f64, (end - start) as f64); }
        self.edit.playhead_was_in_view = target.is_some() || (start..end).contains(&frame);
        target.is_some()
    }

    pub fn drag_marker(&mut self, marker: Marker, drag: &mut rune::Drag) {
        use rune::Phase;
        let Some((start, end, _)) = self.audio_section() else { return };
        let Some((view_start, view_end, _)) = self.audio_view() else { return };
        let width = self.stage_size[0].max(1.0);
        let span = (view_end - view_start).max(1) as f64;
        match drag.phase {
            Phase::Down => {
                let frame = if marker == Marker::Start { start } else { end };
                drag.data = ((frame as f64 - view_start as f64) / span * f64::from(width)) as f32;
            }
            Phase::Move | Phase::Up => {
                let at = (f64::from(drag.data + drag.dx) / f64::from(width)).clamp(0.0, 1.0);
                let mut frame = view_start + (at * span) as u64;
                let done = drag.phase == Phase::Up;
                if done && self.audio_edit().looped && let Some(pcm) = self.audio_source() {
                    frame = sound::snap_to_zero(&pcm, frame);
                }
                match marker {
                    Marker::Start => self.set_section(frame, end, done),
                    Marker::End => self.set_section(start, frame, done),
                }
            }
        }
    }

    pub fn nudge_marker(&mut self, marker: Marker, milliseconds: f32) {
        let Some((start, end, _)) = self.audio_section() else { return };
        let Some(rate) = self.source_format().map(|(rate, _)| rate) else { return };
        let delta = (f64::from(milliseconds) * f64::from(rate) / 1000.0) as i64;
        let moved = |frame: u64| frame.saturating_add_signed(delta);
        match marker {
            Marker::Start => self.set_section(moved(start), end, true),
            Marker::End => self.set_section(start, moved(end), true),
        }
    }

    pub fn clear_section(&mut self) {
        let mut edit = self.audio_edit();
        edit.section = None;
        self.set_audio_edit(edit);
    }

    pub fn set_looped(&mut self, looped: bool) {
        let mut edit = self.audio_edit();
        edit.looped = looped;
        self.set_audio_edit(edit);
        if looped { self.edit.audio_tab = AudioTab::Loop; }
    }

    /// The seam view's drag: pulling the audio sideways slides where the loop ends.
    pub fn drag_seam(&mut self, drag: &rune::Drag) {
        use rune::Phase;
        let Some((start, end, _)) = self.audio_section() else { return };
        let Some(rate) = self.source_format().map(|(rate, _)| rate) else { return };
        if drag.phase == Phase::Down { self.edit.seam_drag_end = end; }
        let frames_per_point = self.edit.seam_ms / 1000.0 * rate as f32 * 2.0 / drag.w.max(1.0);
        let moved = self.edit.seam_drag_end.saturating_add_signed((-drag.dx * frames_per_point) as i64);
        self.set_section(start, moved, drag.phase == Phase::Up);
    }

    pub fn zoom_seam(&mut self, wheel: f32) {
        self.edit.seam_ms = (self.edit.seam_ms * (-wheel * 0.0035).exp()).clamp(2.0, 500.0);
        self.redraw_seam();
    }

    pub fn seam_window_ms(&self) -> f32 {
        self.edit.seam_ms
    }

    pub(crate) fn redraw_seam(&mut self) {
        let Some(pcm) = self.audio_source() else { return };
        let (start, end) = self.audio_edit().bounds(pcm.frames());
        let window = (f64::from(self.edit.seam_ms) / 1000.0 * f64::from(pcm.rate)).max(2.0) as u64;
        let dark = self.resolved_dark.get();
        let (left, right) = if dark { ([150, 150, 158], [242, 242, 242]) } else { ([120, 120, 128], [22, 22, 24]) };
        let px = sound::draw_seam(&pcm, start, end, window, SEAM_W, SEAM_H, left, right);
        self.edit.seam.set(SEAM_W, SEAM_H, px);
    }

    /// Looks near the loop's end for the point that joins most smoothly to its start.
    pub fn tune_seam(&mut self) {
        let Some(asset) = self.selected_asset() else { return };
        let Some(pcm) = self.audio_source() else { return };
        let (start, end) = self.audio_edit().bounds(pcm.frames());
        self.edit.tuning.request((asset.key, pcm), move |(key, pcm)| (key, sound::find_seam(&pcm, start, end)));
    }

    /// What to play for the current state of the edit, and where it sits in the source.
    fn audition(&self) -> Option<(Arc<Pcm>, Timeline)> {
        let source = self.audio_source()?;
        let edit = self.audio_edit();
        let edited = self.selected_key().is_some_and(|k| self.edit.is_edited(k));
        if !edited {
            return Some((source.clone(), Timeline { start: 0.0, length: source.seconds(), looped: false }));
        }
        let start = edit.bounds(source.frames()).0 as f32 / source.rate as f32;
        let pcm = if self.edit.comparing { self.edit.clean.clone() } else { self.edit.coded.clone().or_else(|| self.edit.clean.clone()) }?;
        let length = pcm.seconds();
        Some((pcm, Timeline { start, length, looped: edit.looped }))
    }

    /// Plays the edit from `local` seconds into the clip; false if there's nothing to play yet.
    pub(crate) fn play_audition(&mut self, local: f32) -> bool {
        let Some((pcm, timeline)) = self.audition() else { return false };
        let seconds = local.clamp(0.0, (timeline.length - 0.001).max(0.0));
        self.audio.play_clip(pcm, timeline.looped, seconds);
        self.edit.timeline = Some(timeline);
        self.audio_watch_until = Some(Instant::now() + Duration::from_millis(600));
        true
    }

    /// Swaps what's playing for the latest edit without losing the place.
    fn refresh_audition(&mut self) {
        let waiting = std::mem::take(&mut self.edit.autoplay_waiting);
        if !waiting && !self.audio.is_playing() { return }
        let was_at = self.playback_time();
        let Some((_, timeline)) = self.audition() else {
            self.edit.autoplay_waiting = waiting;
            return;
        };
        self.play_audition(if waiting { 0.0 } else { was_at - timeline.start });
    }

    /// Where playback is, in the source's own seconds.
    pub(crate) fn playback_time(&self) -> f32 {
        let raw = self.audio.position().unwrap_or(0.0);
        self.edit.timeline.map_or(raw, |t| t.source_time(raw))
    }

    /// Moves playback to `at` seconds in the source's own timeline. True if it was handled.
    pub(crate) fn seek_audition(&mut self, at: f32) -> bool {
        let Some((_, timeline)) = self.audition() else { return false };
        let local = (at - timeline.start).clamp(0.0, (timeline.length - 0.001).max(0.0));
        if self.audio.is_finished() || self.edit.timeline.is_none() || self.edit.timeline != Some(timeline) {
            self.play_audition(local);
        } else {
            self.audio.seek(local);
        }
        true
    }

    /// Starts just before the loop's end, to hear it go round.
    pub fn hear_seam(&mut self, tx: &mut rune::Tx) {
        let Some((_, timeline)) = self.audition() else { return };
        self.play_audition((timeline.length - SEAM_LEAD_SECONDS).max(0.0));
        tx.wake_after(Duration::from_millis(33));
    }

    /// Text for the drag bar under an edited asset.
    pub fn drag_detail(&self, asset: &Asset, unedited: String) -> String {
        if !self.edit.is_edited(asset.key) { return unedited }
        match &self.edit.output {
            Some(output) => {
                let ext = output.name.rsplit('.').next().unwrap_or("").to_uppercase();
                format!("{ext}, {}", crate::app::bytes(output.bytes))
            }
            None => "Preparing".to_owned(),
        }
    }

    /// Whether the texture on stage should come from the render, used when a preview lands.
    pub(crate) fn editor_after_preview(&mut self) {
        if matches!(self.stage.scene, Scene::Texture(_)) { self.show_rendered_image(); }
    }
}
