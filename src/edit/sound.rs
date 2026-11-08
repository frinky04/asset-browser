//! Cutting, looping and compressing audio. Everything leaves as Ogg Vorbis.

use std::f32::consts::{FRAC_PI_2, PI};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

/// Decoded audio stays in memory while it's edited, so very long recordings are turned away.
const MAX_MINUTES: u64 = 20;
pub const DEFAULT_QUALITY: u8 = 5;

/// Interleaved samples.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub rate: u32,
}

impl Pcm {
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64 / u64::from(self.channels.max(1))
    }

    pub fn seconds(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1) as f32
    }

    /// One channel-averaged sample per frame, for frames in `from..to`.
    fn mono(&self, from: u64, to: u64) -> Vec<f32> {
        let channels = usize::from(self.channels.max(1));
        self.samples[from as usize * channels..to as usize * channels].chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioEdit {
    /// The frames that are kept, end exclusive; `None` keeps them all.
    pub section: Option<(u64, u64)>,
    /// Make the section repeat cleanly.
    pub looped: bool,
    /// How long the seam of a loop is blended over.
    pub crossfade_ms: u32,
    /// Vorbis quality, 0..=10.
    pub quality: u8,
    /// A lower sample rate to leave at.
    pub rate: Option<u32>,
    pub mono: bool,
}

impl Default for AudioEdit {
    fn default() -> Self {
        AudioEdit { section: None, looped: false, crossfade_ms: 0, quality: DEFAULT_QUALITY, rate: None, mono: false }
    }
}

impl AudioEdit {
    /// The section in frames of a source `frames` long, never empty or past the end.
    pub fn bounds(&self, frames: u64) -> (u64, u64) {
        let (start, end) = self.section.unwrap_or((0, frames));
        let end = end.min(frames);
        (start.min(end.saturating_sub(1)), end.max(1))
    }
}

fn read_stream(stream: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Pcm, String> {
    let stream = MediaSourceStream::new(stream, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension { hint.with_extension(ext); }
    // Gapless, so a loop comes back exactly as long as it went in.
    let options = FormatOptions { enable_gapless: true, ..FormatOptions::default() };
    let mut format = symphonia::default::get_probe().format(&hint, stream, &options, &MetadataOptions::default()).map_err(|e| e.to_string())?.format;
    let track = format.default_track().ok_or("the file has no audio track")?;
    let (track_id, params) = (track.id, track.codec_params.clone());
    let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    let (mut rate, mut channels) = (params.sample_rate.unwrap_or(0), params.channels.map_or(0, |c| c.count() as u16));
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id { continue }
        let Ok(audio) = decoder.decode(&packet) else { continue };
        let spec = *audio.spec();
        rate = spec.rate;
        channels = spec.channels.count() as u16;
        let mut buffer = SampleBuffer::<f32>::new(audio.capacity() as u64, spec);
        buffer.copy_interleaved_ref(audio);
        samples.extend_from_slice(buffer.samples());
        if samples.len() as u64 / u64::from(channels.max(1)) > u64::from(rate) * MAX_MINUTES * 60 {
            return Err(format!("it's too long to edit here (over {MAX_MINUTES} minutes)"));
        }
    }
    if samples.is_empty() || rate == 0 || channels == 0 { return Err("the file has no audio".to_owned()); }
    Ok(Pcm { samples, channels, rate })
}

pub fn decode_file(path: &Path) -> Result<Pcm, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    read_stream(Box::new(file), path.extension().and_then(|e| e.to_str()))
}

pub fn decode_bytes(bytes: Vec<u8>, extension: &str) -> Result<Pcm, String> {
    read_stream(Box::new(Cursor::new(bytes)), Some(extension))
}

/// A 0..1 point on the equal-power fade, as (incoming, outgoing) gains.
fn fade(at: f32) -> (f32, f32) {
    ((at * FRAC_PI_2).sin(), (at * FRAC_PI_2).cos())
}

/// The kept section. A loop with a crossfade blends its seam using audio from just outside the
/// section where there is some, so it keeps its length; a section with no spare audio either
/// side overlaps its own ends and comes out one crossfade shorter.
pub fn cut(source: &Pcm, edit: &AudioEdit) -> Pcm {
    let channels = usize::from(source.channels.max(1));
    let total = source.frames() as usize;
    let (start, end) = edit.bounds(source.frames());
    let (start, end) = (start as usize, end as usize);
    let length = end - start;
    let fade_frames = if edit.looped { (edit.crossfade_ms as usize * source.rate as usize / 1000).min(length / 2) } else { 0 };
    let at = |frame: usize| &source.samples[frame * channels..(frame + 1) * channels];
    let mut out = source.samples[start * channels..end * channels].to_vec();

    let blend = |into: &mut [f32], incoming: &[f32], outgoing: &[f32], step: usize| {
        let (gain_in, gain_out) = fade((step as f32 + 0.5) / fade_frames as f32);
        for ((slot, a), b) in into.iter_mut().zip(incoming).zip(outgoing) { *slot = a * gain_in + b * gain_out; }
    };
    if fade_frames > 0 {
        if total - end >= fade_frames {
            // What follows the section fades out over its start, so its end runs straight on.
            for i in 0..fade_frames { blend(&mut out[i * channels..(i + 1) * channels], at(start + i), at(end + i), i); }
        } else if start >= fade_frames {
            // What comes before the section fades in over its end, leading straight into its start.
            for i in 0..fade_frames { blend(&mut out[(length - fade_frames + i) * channels..(length - fade_frames + i + 1) * channels], at(start - fade_frames + i), at(end - fade_frames + i), i); }
        } else {
            for i in 0..fade_frames { blend(&mut out[i * channels..(i + 1) * channels], at(start + i), at(end - fade_frames + i), i); }
            out.truncate((length - fade_frames) * channels);
        }
    }
    Pcm { samples: out, channels: source.channels, rate: source.rate }
}

/// Applies the output's channel count and sample rate.
pub fn convert(pcm: Pcm, edit: &AudioEdit) -> Pcm {
    let pcm = if edit.mono && pcm.channels > 1 {
        Pcm { samples: pcm.mono(0, pcm.frames()), channels: 1, rate: pcm.rate }
    } else {
        pcm
    };
    match edit.rate {
        Some(rate) if rate < pcm.rate => resample(&pcm, rate, edit.looped),
        _ => pcm,
    }
}

/// Band-limited downsampling with a Hann-windowed sinc. A loop's filter reads across its seam.
fn resample(pcm: &Pcm, rate: u32, wrap: bool) -> Pcm {
    const ZEROS: f32 = 16.0;
    let channels = usize::from(pcm.channels.max(1));
    let frames = pcm.frames() as i64;
    let ratio = rate as f32 / pcm.rate as f32;
    let cutoff = ratio * 0.95;
    let radius = (ZEROS / cutoff).ceil() as i64;
    let out_frames = ((frames as f64 * f64::from(rate) / f64::from(pcm.rate)).round() as usize).max(1);
    let mut out = vec![0.0f32; out_frames * channels];
    for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
        let center = index as f64 * f64::from(pcm.rate) / f64::from(rate);
        let nearest = center.floor() as i64;
        let mut weight_sum = 0.0;
        for k in nearest - radius..=nearest + radius {
            let source = if wrap { k.rem_euclid(frames) } else if (0..frames).contains(&k) { k } else { continue };
            let distance = (k as f64 - center) as f32;
            let x = distance * cutoff;
            let sinc = if x.abs() < 1e-5 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let window = 0.5 + 0.5 * (PI * distance / (radius as f32 + 1.0)).cos();
            let weight = sinc * window;
            weight_sum += weight;
            let from = &pcm.samples[source as usize * channels..(source as usize + 1) * channels];
            for (slot, sample) in frame.iter_mut().zip(from) { *slot += sample * weight; }
        }
        if weight_sum.abs() > 1e-6 { for slot in frame.iter_mut() { *slot /= weight_sum; } }
    }
    Pcm { samples: out, channels: pcm.channels, rate }
}

pub fn encode(pcm: &Pcm, quality: u8) -> Result<Vec<u8>, String> {
    let rate = NonZeroU32::new(pcm.rate).ok_or("no sample rate")?;
    let channels = NonZeroU8::new(u8::try_from(pcm.channels).map_err(|_| "too many channels")?).ok_or("no channels")?;
    // A fixed serial, so the same edit always makes the same bytes.
    let mut builder = VorbisEncoderBuilder::new_with_serial(rate, channels, Vec::new(), 0x4142_4544);
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr { target_quality: f32::from(quality.min(10)) / 10.0 });
    let mut encoder = builder.build().map_err(|e| e.to_string())?;
    let count = usize::from(pcm.channels);
    let mut planes = vec![Vec::new(); count];
    for block in pcm.samples.chunks(4096 * count) {
        for plane in &mut planes { plane.clear(); }
        for frame in block.chunks_exact(count) {
            for (plane, sample) in planes.iter_mut().zip(frame) { plane.push(sample.clamp(-1.0, 1.0)); }
        }
        encoder.encode_audio_block(&planes).map_err(|e| e.to_string())?;
    }
    encoder.finish().map_err(|e| e.to_string())
}

/// `frame` moved to the nearest place within 5 ms where the waveform crosses zero going up.
pub fn snap_to_zero(pcm: &Pcm, frame: u64) -> u64 {
    let reach = u64::from(pcm.rate / 200).max(1);
    let (from, to) = (frame.saturating_sub(reach), (frame + reach).min(pcm.frames()));
    if to < from + 2 { return frame }
    let mono = pcm.mono(from, to);
    let centre = (frame - from) as i64;
    mono.windows(2).enumerate()
        .filter(|(_, pair)| pair[0] <= 0.0 && pair[1] > 0.0)
        .map(|(i, _)| i as i64 + 1)
        .min_by_key(|i| (i - centre).abs())
        .map_or(frame, |i| from + i as u64)
}

/// The end nearest the given one whose surrounding audio best matches the start's, looking up
/// to 125 ms either way.
pub fn find_seam(pcm: &Pcm, start: u64, end: u64) -> u64 {
    let reach = i64::from(pcm.rate / 8);
    let window = i64::from(pcm.rate / 25);
    let frames = pcm.frames() as i64;
    let (start_i, end_i) = (start as i64, end as i64);
    let mono = pcm.mono(0, pcm.frames());
    let lowest = (start_i + window * 2).max(end_i - reach);
    let highest = frames.min(end_i + reach);
    let mut best = (f32::MIN, end_i);
    for candidate in lowest..=highest {
        let before = window.min(start_i).min(candidate);
        let after = window.min(frames - start_i).min(frames - candidate);
        let a = &mono[(start_i - before) as usize..(start_i + after) as usize];
        let b = &mono[(candidate - before) as usize..(candidate + after) as usize];
        let (mut matched, mut energy_a, mut energy_b) = (0.0f32, 0.0f32, 0.0f32);
        for (x, y) in a.iter().zip(b) {
            matched += x * y;
            energy_a += x * x;
            energy_b += y * y;
        }
        let score = matched / (energy_a * energy_b).sqrt().max(1e-9);
        // Ties (a pure tone repeats exactly) go to the end nearest the one asked about.
        let better = score > best.0 + 1e-4 || ((score - best.0).abs() <= 1e-4 && (candidate - end_i).abs() < (best.1 - end_i).abs());
        if better { best = (score, candidate); }
    }
    best.1 as u64
}

/// The waveform around a loop's seam: the frames before `end` on the left half, the frames from
/// `start` on the right, `window` frames each, as straight-alpha RGBA.
#[allow(clippy::too_many_arguments)]
pub fn draw_seam(pcm: &Pcm, start: u64, end: u64, window: u64, width: u32, height: u32, left: [u8; 3], right: [u8; 3]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let frames = pcm.frames();
    let before = pcm.mono(end.saturating_sub(window).min(frames), end.min(frames));
    let after = pcm.mono(start.min(frames), (start + window).min(frames));
    let mid = (height as f32 - 1.0) * 0.5;
    let half = w / 2;
    let peak = before.iter().chain(&after).fold(0.05f32, |m, v| m.max(v.abs()));
    for x in 0..w {
        let (data, color, column, columns, missing) = if x < half {
            (&before, left, x, half, window as usize - before.len())
        } else {
            (&after, right, x - half, w - half, 0)
        };
        let span = window as f32 / columns.max(1) as f32;
        let from = column as f32 * span - missing as f32;
        let to = from + span;
        let (lo, hi) = (from.floor().max(0.0) as usize, to.ceil().max(0.0) as usize + 1);
        if lo >= data.len() { continue }
        let slice = &data[lo..hi.min(data.len())];
        if slice.is_empty() { continue }
        let (mut low, mut high) = (f32::MAX, f32::MIN);
        for v in slice {
            low = low.min(*v);
            high = high.max(*v);
        }
        let top = (mid - high / peak * (mid - 1.0)).floor() as usize;
        let bottom = (mid - low / peak * (mid - 1.0)).ceil() as usize;
        for y in top.min(h - 1)..=bottom.min(h - 1) {
            let at = (y * w + x) * 4;
            out[at..at + 3].copy_from_slice(&color);
            out[at + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> Pcm {
        Pcm { samples: (0..frames).map(|i| i as f32).collect(), channels: 1, rate: 1000 }
    }

    fn sine(frames: usize, rate: u32, hz: f32, channels: u16) -> Pcm {
        let samples = (0..frames).flat_map(|i| {
            let v = 0.5 * (i as f32 / rate as f32 * hz * 2.0 * PI).sin();
            std::iter::repeat_n(v, usize::from(channels))
        }).collect();
        Pcm { samples, channels, rate }
    }

    #[test]
    fn a_section_keeps_exactly_those_frames() {
        let out = cut(&ramp(100), &AudioEdit { section: Some((10, 30)), ..AudioEdit::default() });
        assert_eq!(out.samples, (10..30).map(|i| i as f32).collect::<Vec<_>>());
    }

    #[test]
    fn a_crossfaded_loop_runs_on_past_its_end_into_its_start() {
        let source = ramp(1000);
        let out = cut(&source, &AudioEdit { section: Some((100, 500)), looped: true, crossfade_ms: 20, ..AudioEdit::default() });
        assert_eq!(out.frames(), 400, "audio after the section pays for the fade");
        // The start of the loop now begins as what would have followed its end.
        assert!((out.samples[0] - 500.0).abs() < 10.0, "{}", out.samples[0]);
        assert_eq!(*out.samples.last().unwrap(), 499.0);
    }

    #[test]
    fn a_loop_at_the_end_of_a_file_borrows_from_before_its_start() {
        let source = ramp(1000);
        let out = cut(&source, &AudioEdit { section: Some((100, 1000)), looped: true, crossfade_ms: 20, ..AudioEdit::default() });
        assert_eq!(out.frames(), 900);
        assert_eq!(out.samples[0], 100.0);
        // The last frame is still 4% the audio it's leaving, and on this ramp that's far away.
        assert!((out.samples[899] - 99.0).abs() < 45.0, "ends as what led into its start: {}", out.samples[899]);
    }

    #[test]
    fn a_loop_of_the_whole_file_overlaps_itself() {
        let out = cut(&ramp(1000), &AudioEdit { looped: true, crossfade_ms: 20, ..AudioEdit::default() });
        assert_eq!(out.frames(), 980);
        assert!((out.samples[0] - 980.0).abs() < 10.0, "{}", out.samples[0]);
        assert_eq!(*out.samples.last().unwrap(), 979.0);
    }

    #[test]
    fn bad_sections_are_clamped_rather_than_crashing() {
        assert_eq!(AudioEdit { section: Some((90, 500)), ..AudioEdit::default() }.bounds(100), (90, 100));
        assert_eq!(AudioEdit { section: Some((500, 600)), ..AudioEdit::default() }.bounds(100), (99, 100));
    }

    #[test]
    fn vorbis_round_trips_the_same_length_and_better_quality_is_bigger() {
        let source = sine(44_100, 44_100, 440.0, 2);
        let low = encode(&source, 0).unwrap();
        let high = encode(&source, 10).unwrap();
        assert!(high.len() > low.len(), "{} against {}", high.len(), low.len());
        let heard = decode_bytes(high, "ogg").unwrap();
        assert_eq!((heard.frames(), heard.channels, heard.rate), (44_100, 2, 44_100));
    }

    #[test]
    fn encoding_the_same_audio_twice_makes_the_same_file() {
        let source = sine(11_025, 44_100, 330.0, 1);
        assert_eq!(encode(&source, 3).unwrap(), encode(&source, 3).unwrap());
    }

    #[test]
    fn resampling_keeps_length_and_the_tone_but_removes_what_cannot_fit() {
        let keep = convert(sine(48_000, 48_000, 200.0, 1), &AudioEdit { rate: Some(16_000), ..AudioEdit::default() });
        assert_eq!((keep.frames(), keep.rate), (16_000, 16_000));
        let loudest = keep.samples[100..15_900].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((loudest - 0.5).abs() < 0.02, "{loudest}");
        // A tone above the new Nyquist must vanish, not fold down as an alias.
        let gone = convert(sine(48_000, 48_000, 12_000.0, 1), &AudioEdit { rate: Some(16_000), ..AudioEdit::default() });
        let leftover = gone.samples[100..15_900].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(leftover < 0.02, "{leftover}");
    }

    #[test]
    fn mono_averages_the_channels() {
        let source = Pcm { samples: vec![1.0, 0.0, 0.5, 0.5], channels: 2, rate: 1000 };
        assert_eq!(convert(source, &AudioEdit { mono: true, ..AudioEdit::default() }).samples, [0.5, 0.5]);
    }

    #[test]
    fn snapping_finds_the_nearest_upward_crossing() {
        let pcm = sine(1000, 1000, 100.0, 1);
        // The tone rises through zero every ten frames.
        assert_eq!(snap_to_zero(&pcm, 22) % 10, 0, "{}", snap_to_zero(&pcm, 22));
    }

    #[test]
    fn finding_a_seam_lands_on_a_whole_number_of_cycles() {
        let pcm = sine(44_100, 44_100, 100.0, 1);
        let start = 4_410;
        let end = find_seam(&pcm, start, 30_123);
        assert_eq!((end - start) % 441, 0, "{end}");
        assert!((end as i64 - 30_123).abs() <= 441, "stays near where it was asked: {end}");
    }

    #[test]
    fn the_seam_view_draws_both_sides() {
        let pcm = sine(1000, 1000, 50.0, 1);
        let px = draw_seam(&pcm, 100, 900, 40, 40, 20, [1, 2, 3], [4, 5, 6]);
        assert_eq!(px.len(), 40 * 20 * 4);
        let colours: std::collections::HashSet<u8> = px.chunks_exact(4).filter(|p| p[3] == 255).map(|p| p[0]).collect();
        assert_eq!(colours, [1, 4].into_iter().collect());
    }
}
