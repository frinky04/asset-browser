//! Reading audio into peaks, and drawing them.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One channel-summed amplitude per bucket, 0..=1 before normalizing.
pub struct Peaks {
    pub values: Vec<f32>,
    pub seconds: f32,
    pub rate: u32,
    pub channels: u16,
}

/// How much of each bucket a long WAV reads: enough to see its shape without reading 30 GB.
const WINDOW_BYTES: u64 = 32 * 1024;

struct WavInfo {
    format: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    data_at: u64,
    data_len: u64,
}

fn parse_wav(file: &mut File) -> Option<WavInfo> {
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" { return None; }
    let file_len = file.metadata().ok()?.len();
    let (mut fmt, mut at) = (None, 12u64);
    while at + 8 <= file_len {
        file.seek(SeekFrom::Start(at)).ok()?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let len = u64::from(u32::from_le_bytes([head[4], head[5], head[6], head[7]]));
        match &head[0..4] {
            b"fmt " => {
                let mut f = vec![0u8; len.min(64) as usize];
                file.read_exact(&mut f).ok()?;
                if f.len() < 16 { return None; }
                let mut format = u16::from_le_bytes([f[0], f[1]]);
                if format == 0xFFFE && f.len() >= 26 { format = u16::from_le_bytes([f[24], f[25]]); }
                fmt = Some((
                    format,
                    u16::from_le_bytes([f[2], f[3]]),
                    u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                    u16::from_le_bytes([f[14], f[15]]),
                ));
            }
            b"data" => {
                let (format, channels, rate, bits) = fmt?;
                let supported = (format == 1 && matches!(bits, 8 | 16 | 24 | 32)) || (format == 3 && bits == 32);
                if !supported || channels == 0 || rate == 0 { return None; }
                return Some(WavInfo { format, channels, rate, bits, data_at: at + 8, data_len: len.min(file_len - (at + 8)) });
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    None
}

fn sample(info: &WavInfo, b: &[u8]) -> f32 {
    match (info.format, info.bits) {
        (3, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        (_, 8) => (f32::from(b[0]) - 128.0) / 128.0,
        (_, 16) => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
        (_, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
        _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
    }
}

fn wav_peaks(file: &mut File, info: &WavInfo, buckets: usize) -> Option<Peaks> {
    let frame = u64::from(info.bits / 8) * u64::from(info.channels);
    let frames = info.data_len / frame;
    if frames == 0 { return None; }
    let per_bucket = (frames as f64 / buckets as f64).max(1.0);
    let mut values = vec![0f32; buckets];
    let mut buf = Vec::new();
    for (i, value) in values.iter_mut().enumerate() {
        let start = (i as f64 * per_bucket) as u64;
        if start >= frames { break }
        let want = (per_bucket.ceil() as u64 * frame).min(WINDOW_BYTES / frame * frame).max(frame);
        buf.resize(want.min(info.data_len - start * frame) as usize, 0);
        file.seek(SeekFrom::Start(info.data_at + start * frame)).ok()?;
        file.read_exact(&mut buf).ok()?;
        let width = (info.bits / 8) as usize;
        *value = buf.chunks_exact(width).map(|b| sample(info, b).abs()).fold(0.0, f32::max);
    }
    Some(Peaks { values, seconds: frames as f32 / info.rate as f32, rate: info.rate, channels: info.channels })
}

fn decoded_peaks(path: &Path, buckets: usize) -> Result<Peaks, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) { hint.with_extension(ext); }
    let mut format = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?.format;
    let track = format.default_track().ok_or("the file has no audio track")?;
    let (track_id, params) = (track.id, track.codec_params.clone());
    let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    let (mut rate, mut channels) = (params.sample_rate.unwrap_or(0), params.channels.map_or(0, |c| c.count() as u16));
    let mut blocks = Vec::new();
    let mut frames = 0u64;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id { continue }
        let Ok(audio) = decoder.decode(&packet) else { continue };
        let spec = *audio.spec();
        rate = spec.rate;
        channels = spec.channels.count() as u16;
        let mut samples = SampleBuffer::<f32>::new(audio.capacity() as u64, spec);
        samples.copy_interleaved_ref(audio);
        blocks.push(samples.samples().iter().map(|v| v.abs()).fold(0.0, f32::max));
        frames += (samples.samples().len() / usize::from(channels.max(1))) as u64;
    }
    if blocks.is_empty() { return Err("the file has no audio".into()); }
    let mut values = vec![0f32; buckets];
    for (i, v) in blocks.iter().enumerate() {
        let slot = i * buckets / blocks.len();
        values[slot] = values[slot].max(*v);
    }
    Ok(Peaks { values, seconds: frames as f32 / rate.max(1) as f32, rate, channels })
}

pub fn peaks(path: &Path, buckets: usize) -> Result<Peaks, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    if let Some(p) = parse_wav(&mut file).and_then(|info| wav_peaks(&mut file, &info, buckets)) { return Ok(p); }
    decoded_peaks(path, buckets)
}

/// Mirrored bars across `width`×`height` straight-alpha RGBA, scaled to the loudest moment.
pub fn draw(values: &[f32], width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let loudest = values.iter().copied().fold(0.0, f32::max).max(1e-4);
    let mid = height as f32 * 0.5;
    for x in 0..w {
        let from = x * values.len() / w;
        let to = ((x + 1) * values.len() / w).max(from + 1).min(values.len());
        let v = values[from..to].iter().copied().fold(0.0, f32::max) / loudest;
        // A little compression, so quiet tails stay readable.
        let extent = (v.powf(0.7) * (mid - 1.0)).max(0.75);
        for y in 0..h {
            let d = (y as f32 + 0.5 - mid).abs();
            let a = (extent - d + 0.5).clamp(0.0, 1.0);
            if a > 0.0 {
                let at = (y * w + x) * 4;
                out[at..at + 3].copy_from_slice(&color);
                out[at + 3] = (a * 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
        let mut v = Vec::new();
        let data_len = samples.len() as u32 * 2;
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len + 12).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        // An unrelated chunk before the audio, as broadcast WAVs have.
        v.extend_from_slice(b"bext");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&[0; 4]);
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples { v.extend_from_slice(&s.to_le_bytes()); }
        v
    }

    #[test]
    fn wav_peaks_find_the_loud_half_past_other_chunks() {
        let mut samples = vec![100i16; 500];
        samples.extend(vec![20000i16; 500]);
        let dir = std::env::temp_dir().join(format!("ab-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.wav");
        std::fs::write(&file, wav(&samples, 1000)).unwrap();
        let p = peaks(&file, 2).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!((p.seconds - 1.0).abs() < 1e-3, "{}", p.seconds);
        assert!(p.values[0] < 0.01 && p.values[1] > 0.5, "{:?}", p.values);
    }

    #[test]
    fn drawing_keeps_the_size_and_centres_the_bars() {
        let px = draw(&[1.0, 0.0], 4, 10, [9, 9, 9]);
        assert_eq!(px.len(), 4 * 10 * 4);
        assert_eq!(px[(5 * 4) * 4 + 3], 255, "loud column at the middle");
        assert_eq!(px[4 * 4 + 3], 255, "and nearly to the edge at full scale");
        assert_eq!(px[3], 0, "leaving a pixel clear");
    }
}
