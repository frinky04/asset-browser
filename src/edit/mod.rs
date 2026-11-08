//! Non-destructive edits. An [`Edit`] is plain data describing what should happen to an asset on
//! its way out; [`render`] turns a decoded source and that data into the bytes that leave. The
//! preview and the drag both come from `render`, so what's previewed is what gets dropped, and
//! nothing here ever writes near the library.

pub mod ball;
pub mod mesh;
pub mod picture;
pub mod sound;

use std::path::Path;
use std::sync::Arc;

use crate::export::Options;
use crate::library::Kind;
use crate::model::Image;

pub use mesh::{MaterialEdit, ModelEdit, ModelDoc, Surface};
pub use picture::{Effort, ImageEdit};
pub use sound::{AudioEdit, Pcm};

/// What to do to one asset. Each kind has one output format, whatever it came in as.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Image(ImageEdit),
    Audio(AudioEdit),
    Model(ModelEdit),
}

impl Edit {
    /// Whether this asks for nothing, so the asset can leave as it is.
    pub fn is_unchanged(&self) -> bool {
        match self {
            // Dithering means nothing without a palette, nor a crossfade without a loop.
            Edit::Image(e) => e.colors.is_none() && e.effort == Effort::Balanced && !e.flip_green,
            Edit::Audio(e) => *e == AudioEdit { crossfade_ms: e.crossfade_ms, ..AudioEdit::default() },
            Edit::Model(e) => e.is_unchanged(),
        }
    }

    /// A stable text form, for telling staged copies of different edits apart.
    pub fn fingerprint(&self) -> String {
        format!("{self:?}")
    }
}

/// A source held in memory for as long as it's being edited.
pub enum Decoded {
    Image(image::RgbaImage),
    Audio(Arc<Pcm>),
    Model(Box<ModelDoc>),
}

/// What a finished render looks or sounds like, for the preview.
pub enum Shown {
    Image(Arc<Image>),
    Audio(Arc<Pcm>),
    Model(Vec<Arc<Image>>),
}

/// The output file, and how to preview it.
pub struct Rendered {
    pub name: String,
    pub bytes: Vec<u8>,
    pub shown: Shown,
}

pub fn open(path: &Path, kind: Kind, library: &Path) -> Result<Decoded, String> {
    match kind {
        Kind::Texture => {
            let reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
            Ok(Decoded::Image(reader.decode().map_err(|e| e.to_string())?.into_rgba8()))
        }
        Kind::Audio => sound::decode_file(path).map(|pcm| Decoded::Audio(Arc::new(pcm))),
        Kind::Model => mesh::open(path, library).map(|doc| Decoded::Model(Box::new(doc))),
    }
}

/// `name` with its extension replaced by `ext`, and `tag` added to the stem.
fn output_name(name: &str, tag: Option<u32>, ext: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    match tag {
        Some(tag) => format!("{stem}_{tag}.{ext}"),
        None => format!("{stem}.{ext}"),
    }
}

/// Runs `options.edit` over `decoded`. `name` is the asset's file name.
pub fn render(decoded: &Decoded, name: &str, options: &Options) -> Result<Rendered, String> {
    let edit = options.edit.as_deref().ok_or("there's no edit to render")?;
    match (decoded, edit) {
        (Decoded::Image(source), Edit::Image(edit)) => {
            let longest = source.width().max(source.height());
            let max = options.max_size.filter(|m| *m < longest);
            let mut sized = match max {
                Some(max) => crate::export::scale(&image::DynamicImage::ImageRgba8(source.clone()), max, options.nearest).into_rgba8(),
                None => source.clone(),
            };
            if edit.flip_green { picture::invert_green(&mut sized); }
            let (bytes, shown) = picture::encode(&sized, *edit)?;
            let (width, height) = shown.dimensions();
            Ok(Rendered { name: output_name(name, max, "png"), bytes, shown: Shown::Image(Arc::new(Image { width, height, rgba: shown.into_raw() })) })
        }
        (Decoded::Audio(source), Edit::Audio(edit)) => {
            let cut = sound::cut(source, edit);
            let converted = sound::convert(cut, edit);
            let bytes = sound::encode(&converted, edit.quality)?;
            let mut heard = sound::decode_bytes(bytes.clone(), "ogg")?;
            // The file ends exactly on its granule position, but the decoder hands back the
            // encoder's padding too, which would be an audible gap in a loop.
            heard.samples.truncate(converted.samples.len());
            Ok(Rendered { name: output_name(name, None, "ogg"), bytes, shown: Shown::Audio(Arc::new(heard)) })
        }
        (Decoded::Model(doc), Edit::Model(edit)) => {
            let (bytes, images) = doc.render(edit, options.nearest)?;
            Ok(Rendered { name: output_name(name, None, "glb"), bytes, shown: Shown::Model(images) })
        }
        _ => Err("the edit doesn't match the asset".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_previewed_loop_is_exactly_as_long_as_the_file_so_it_has_no_gap() {
        let samples = (0..46_000).map(|i| (i as f32 * 0.03).sin() * 0.4).collect();
        let decoded = Decoded::Audio(Arc::new(Pcm { samples, channels: 1, rate: 44_100 }));
        let edit = AudioEdit { section: Some((0, 45_796)), looped: true, ..AudioEdit::default() };
        let options = Options { edit: Some(Arc::new(Edit::Audio(edit))), ..Options::default() };
        let rendered = render(&decoded, "a.wav", &options).unwrap();
        assert_eq!(rendered.name, "a.ogg");
        let Shown::Audio(heard) = rendered.shown else { panic!("audio edits preview as audio") };
        assert_eq!(heard.frames(), 45_796);
    }

    #[test]
    fn flipping_green_inverts_only_green_in_the_output() {
        let decoded = Decoded::Image(image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 60, 255, 255])));
        let options = Options { edit: Some(Arc::new(Edit::Image(ImageEdit { flip_green: true, ..ImageEdit::default() }))), ..Options::default() };
        let rendered = render(&decoded, "n.jpg", &options).unwrap();
        assert_eq!(rendered.name, "n.png");
        let out = image::load_from_memory(&rendered.bytes).unwrap().into_rgba8();
        assert_eq!(out.get_pixel(0, 0).0, [200, 195, 255, 255]);
    }

    #[test]
    fn asking_for_nothing_counts_as_no_edit() {
        assert!(!Edit::Image(ImageEdit { flip_green: true, ..ImageEdit::default() }).is_unchanged());
        assert!(Edit::Audio(AudioEdit { crossfade_ms: 50, ..AudioEdit::default() }).is_unchanged());
        assert!(Edit::Image(ImageEdit { dither: false, ..ImageEdit::default() }).is_unchanged());
        assert!(!Edit::Audio(AudioEdit { quality: 1, ..AudioEdit::default() }).is_unchanged());
        assert!(!Edit::Image(ImageEdit { effort: Effort::Best, ..ImageEdit::default() }).is_unchanged());
    }
}
