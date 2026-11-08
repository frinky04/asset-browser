//! Turns text and pictures into vectors with EmbeddingGemma 2: the same preparation the model's
//! reference processor does, then ONNX Runtime.

use std::path::Path;
use std::sync::OnceLock;

use image::RgbImage;
use image::imageops::{self, FilterType};
use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

use super::install;
use super::vectors::DIMENSIONS;

const PATCH: usize = 16;
const POOLING: usize = 3;
const SOFT_TOKENS: usize = 280;
const MAX_PATCHES: usize = SOFT_TOKENS * POOLING * POOLING;
const CHANNELS: usize = 3;
const PATCH_VALUES: usize = PATCH * PATCH * CHANNELS;

const BOS: i64 = 2;
const EOS: i64 = 1;
const IMAGE_START: i64 = 255_999;
const IMAGE_TOKEN: i64 = 258_880;
const IMAGE_END: i64 = 258_882;
/// The width of the text model, which media tokens are given in.
const HIDDEN: usize = 512;
/// Plenty for anything typed into a search box.
const MOST_QUERY_TOKENS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// This many threads, and no more.
    Cpu(usize),
    Gpu,
}

/// The size a picture is resized to: its shape, as large as the token budget allows, in whole
/// pooled patches.
fn fitted_size(height: usize, width: usize) -> (usize, usize) {
    let target_pixels = (MAX_PATCHES * PATCH * PATCH) as f64;
    let factor = (target_pixels / (height * width) as f64).sqrt();
    let side = POOLING * PATCH;
    let snapped = |length: usize| (factor * length as f64 / side as f64).floor() as usize * side;
    let (h, w) = (snapped(height), snapped(width));
    let longest = MAX_PATCHES / (POOLING * POOLING) * side;
    match (h, w) {
        (0, 0) => (side, side),
        (0, _) => (side, (width / height.max(1) * side).min(longest)),
        (_, 0) => ((height / width.max(1) * side).min(longest), side),
        _ => (h, w),
    }
}

/// A picture as the vision encoder takes it.
#[derive(Debug, PartialEq)]
struct Patches {
    /// `MAX_PATCHES` rows of `PATCH_VALUES`, zero past the picture.
    values: Vec<f32>,
    /// `MAX_PATCHES` rows of column and row, -1 past the picture.
    positions: Vec<i64>,
    soft_tokens: usize,
}

fn patches_of(picture: &RgbImage) -> Patches {
    let (height, width) = (picture.height() as usize, picture.width() as usize);
    let (rows, columns) = (height / PATCH, width / PATCH);
    let raw = picture.as_raw();
    let mut values = vec![0f32; MAX_PATCHES * PATCH_VALUES];
    let mut positions = vec![-1i64; MAX_PATCHES * 2];
    let mut out = 0;
    for row in 0..rows {
        for column in 0..columns {
            let at = (row * columns + column).min(MAX_PATCHES - 1);
            positions[at * 2] = column as i64;
            positions[at * 2 + 1] = row as i64;
            for y in 0..PATCH {
                let start = ((row * PATCH + y) * width + column * PATCH) * CHANNELS;
                for byte in &raw[start..start + PATCH * CHANNELS] {
                    if out < values.len() { values[out] = f32::from(*byte) / 255.0; }
                    out += 1;
                }
            }
        }
    }
    Patches { values, positions, soft_tokens: (rows * columns) / (POOLING * POOLING) }
}

/// The token sequence that holds `soft_tokens` of picture.
fn picture_tokens(soft_tokens: usize) -> Vec<i64> {
    let mut ids = Vec::with_capacity(soft_tokens + 4);
    ids.extend([BOS, IMAGE_START]);
    ids.resize(ids.len() + soft_tokens, IMAGE_TOKEN);
    ids.extend([IMAGE_END, EOS]);
    ids
}

/// Keeps the leading dimensions at unit length, which is how the model is meant to be shortened.
fn shortened(full: &[f32]) -> Vec<f32> {
    let kept = &full[..DIMENSIONS];
    let length = kept.iter().map(|v| v * v).sum::<f32>().sqrt();
    kept.iter().map(|v| v / length.max(f32::MIN_POSITIVE)).collect()
}

fn text_of(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// ONNX Runtime, loaded from the install once per run. It can't be loaded twice.
fn runtime(dir: &Path) -> Result<&'static Environment, String> {
    static RUNTIME: OnceLock<Result<Environment, String>> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        ort::environment::init_from(install::runtime_library(dir)).map_err(text_of)?
            .with_name("asset-browser")
            .with_telemetry(false)
            .build()
            .map_err(text_of)
    }).as_ref().map_err(Clone::clone)
}

fn session(dir: &Path, file: &str, device: Device) -> Result<Session, String> {
    let mut builder = Session::builder(runtime(dir)?).map_err(text_of)?
        .with_inter_threads(1).map_err(text_of)?
        // Waiting threads would otherwise spin, showing as load while doing nothing.
        .with_intra_op_spinning(false).map_err(text_of)?;
    builder = match device {
        Device::Cpu(threads) => builder.with_intra_threads(threads.max(1)).map_err(text_of)?,
        Device::Gpu => builder.with_execution_providers([ort::ep::WebGPU::default().build().error_on_failure()]).map_err(text_of)?,
    };
    builder.commit_from_file(dir.join(file)).map_err(text_of)
}

/// The text model, and the vision encoder for whoever embeds pictures.
pub struct Embedder {
    text: Session,
    vision: Option<Session>,
    tokenizer: Option<tokenizers::Tokenizer>,
}

impl Embedder {
    pub fn for_pictures(dir: &Path, device: Device) -> Result<Embedder, String> {
        Ok(Embedder {
            text: session(dir, "model_quantized.onnx", device)?,
            vision: Some(session(dir, "vision_encoder_quantized.onnx", device)?),
            tokenizer: None,
        })
    }

    pub fn for_queries(dir: &Path, device: Device) -> Result<Embedder, String> {
        let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(text_of)?;
        Ok(Embedder { text: session(dir, "model_quantized.onnx", device)?, vision: None, tokenizer: Some(tokenizer) })
    }

    fn through_text_model(&mut self, ids: Vec<i64>, image_features: (usize, Vec<f32>)) -> Result<Vec<f32>, String> {
        let n = ids.len();
        let none = || Tensor::from_array(([0usize, HIDDEN], Vec::<f32>::new())).map_err(text_of);
        let outputs = self.text.run(ort::inputs! {
            "input_ids" => Tensor::from_array(([1usize, n], ids)).map_err(text_of)?,
            "attention_mask" => Tensor::from_array(([1usize, n], vec![1i64; n])).map_err(text_of)?,
            "image_features" => Tensor::from_array(([image_features.0, HIDDEN], image_features.1)).map_err(text_of)?,
            "video_features" => none()?,
            "audio_features" => none()?,
        }).map_err(text_of)?;
        let (_, full) = outputs["sentence_embedding"].try_extract_tensor::<f32>().map_err(text_of)?;
        Ok(full.to_vec())
    }

    fn full_query(&mut self, query: &str) -> Result<Vec<f32>, String> {
        let tokenizer = self.tokenizer.as_ref().ok_or("no tokenizer loaded")?;
        let encoding = tokenizer.encode(format!("task: search result | query: {query}"), true).map_err(text_of)?;
        let mut ids: Vec<i64> = encoding.get_ids().iter().map(|i| i64::from(*i)).collect();
        if ids.len() > MOST_QUERY_TOKENS {
            ids.truncate(MOST_QUERY_TOKENS - 1);
            ids.push(EOS);
        }
        self.through_text_model(ids, (0, Vec::new()))
    }

    pub fn query(&mut self, query: &str) -> Result<Vec<f32>, String> {
        Ok(shortened(&self.full_query(query)?))
    }

    fn full_picture(&mut self, picture: &RgbImage) -> Result<Vec<f32>, String> {
        if picture.width() == 0 || picture.height() == 0 { return Err("empty picture".to_owned()) }
        let (height, width) = fitted_size(picture.height() as usize, picture.width() as usize);
        let resized;
        let fitted = if (height, width) == (picture.height() as usize, picture.width() as usize) { picture } else {
            resized = imageops::resize(picture, width as u32, height as u32, FilterType::CatmullRom);
            &resized
        };
        let patches = patches_of(fitted);
        let vision = self.vision.as_mut().ok_or("no vision encoder loaded")?;
        let outputs = vision.run(ort::inputs! {
            "pixel_values" => Tensor::from_array(([1usize, MAX_PATCHES, PATCH_VALUES], patches.values)).map_err(text_of)?,
            "pixel_position_ids" => Tensor::from_array(([1usize, MAX_PATCHES, 2], patches.positions)).map_err(text_of)?,
        }).map_err(text_of)?;
        let (shape, features) = outputs["image_features"].try_extract_tensor::<f32>().map_err(text_of)?;
        let tokens = shape.first().copied().unwrap_or(0) as usize;
        if tokens != patches.soft_tokens { return Err(format!("expected {} picture tokens, got {tokens}", patches.soft_tokens)) }
        let features = features.to_vec();
        drop(outputs);
        self.through_text_model(picture_tokens(tokens), (tokens, features))
    }

    pub fn picture(&mut self, picture: &RgbImage) -> Result<Vec<f32>, String> {
        Ok(shortened(&self.full_picture(picture)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_fitted_the_way_the_reference_processor_fits_them() {
        // Figures from the reference processor, which also gave 266 soft tokens for the first.
        assert_eq!(fitted_size(64, 83), (672, 912));
        assert_eq!(fitted_size(512, 768), (624, 960));
        assert_eq!(fitted_size(1024, 1024), (768, 768));
    }

    #[test]
    fn an_extreme_shape_still_gets_a_row_of_patches() {
        let (h, w) = fitted_size(1, 4000);
        assert_eq!(h, POOLING * PATCH);
        assert!(w > 0 && w <= MAX_PATCHES / (POOLING * POOLING) * POOLING * PATCH);
    }

    #[test]
    fn patches_read_across_then_down_and_pad_the_rest() {
        let mut picture = RgbImage::new(96, 48);
        picture.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        picture.put_pixel(16, 0, image::Rgb([0, 255, 0]));
        let p = patches_of(&picture);
        assert_eq!(p.soft_tokens, 2);
        assert_eq!(p.values.len(), MAX_PATCHES * PATCH_VALUES);
        assert_eq!(&p.values[..3], &[1.0, 0.0, 0.0]);
        assert_eq!(&p.values[PATCH_VALUES..PATCH_VALUES + 3], &[0.0, 1.0, 0.0]);
        assert_eq!(&p.positions[..4], &[0, 0, 1, 0]);
        assert_eq!(&p.positions[(6 * 3 - 1) * 2..(6 * 3 - 1) * 2 + 2], &[5, 2]);
        assert_eq!(p.positions[6 * 3 * 2], -1, "past the picture");
    }

    #[test]
    fn a_picture_is_a_run_of_tokens_between_its_markers() {
        let ids = picture_tokens(266);
        assert_eq!(ids.len(), 270);
        assert_eq!(&ids[..3], &[2, 255_999, 258_880]);
        assert_eq!(&ids[ids.len() - 3..], &[258_880, 258_882, 1]);
    }

    #[test]
    fn a_shortened_vector_is_unit_length() {
        let v = vec![2.0; 768];
        let short = shortened(&v);
        assert_eq!(short.len(), DIMENSIONS);
        assert!((short.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (norm(a) * norm(b))
    }

    /// Needs an installed model. `AB_MODEL_DIR` is the install; `AB_GOLDEN_IMAGE` a picture and
    /// `AB_GOLDEN_VECTOR` the 768 little-endian floats the reference processor gave for it.
    #[test]
    #[ignore = "needs the downloaded model"]
    fn vectors_agree_with_the_reference_implementation() {
        let dir = std::path::PathBuf::from(std::env::var("AB_MODEL_DIR").expect("AB_MODEL_DIR"));
        for device in [Device::Cpu(4), Device::Gpu] {
            let mut queries = Embedder::for_queries(&dir, device).unwrap();
            let text = queries.full_query("a wooden door").unwrap();
            assert_eq!(text.len(), 768);
            let reference = [-0.013_272_945f32, 0.023_020_728, 0.010_411_475, -0.020_657_735];
            for (got, want) in text.iter().zip(reference) { assert!((got - want).abs() < 1e-4, "{device:?}: {got} vs {want}"); }

            let picture = image::open(std::env::var("AB_GOLDEN_IMAGE").expect("AB_GOLDEN_IMAGE")).unwrap().to_rgb8();
            let bytes = std::fs::read(std::env::var("AB_GOLDEN_VECTOR").expect("AB_GOLDEN_VECTOR")).unwrap();
            let want: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            let mut pictures = Embedder::for_pictures(&dir, device).unwrap();
            let got = pictures.full_picture(&picture).unwrap();
            let agreement = cosine(&got, &want);
            println!("{device:?}: cosine to the reference {agreement}");
            assert!(agreement > 0.99, "{device:?}: {agreement}");
        }
    }
}
