//! Playback on its own thread: the output stream can't leave the thread that made it, and
//! opening a file must never stall a frame.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{Decoder, OutputStream, Sink, Source};

use crate::edit::Pcm;

enum Command {
    Play(PathBuf),
    /// Audio already in memory, starting `at` seconds in.
    Clip { pcm: Arc<Pcm>, looped: bool, at: f32 },
}

/// Plays samples held in memory, optionally forever.
struct Clip {
    pcm: Arc<Pcm>,
    next: usize,
    looped: bool,
}

impl Clip {
    fn sample_index(pcm: &Pcm, seconds: f32) -> usize {
        let frame = (f64::from(seconds.max(0.0)) * f64::from(pcm.rate)) as u64;
        (frame.min(pcm.frames()) * u64::from(pcm.channels)) as usize
    }
}

impl Iterator for Clip {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.next >= self.pcm.samples.len() {
            if !self.looped || self.pcm.samples.is_empty() { return None }
            self.next = 0;
        }
        let sample = self.pcm.samples[self.next];
        self.next += 1;
        Some(sample)
    }
}

impl Source for Clip {
    fn current_frame_len(&self) -> Option<usize> { None }
    fn channels(&self) -> u16 { self.pcm.channels }
    fn sample_rate(&self) -> u32 { self.pcm.rate }
    fn total_duration(&self) -> Option<Duration> {
        (!self.looped).then(|| Duration::from_secs_f32(self.pcm.seconds()))
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.next = Clip::sample_index(&self.pcm, pos.as_secs_f32());
        Ok(())
    }
}

#[derive(Clone)]
pub struct Audio {
    commands: Sender<Command>,
    sink: Arc<Mutex<Option<Sink>>>,
    /// The output level, as `f32` bits.
    volume: Arc<AtomicU32>,
}

impl Audio {
    pub fn new() -> Audio {
        let (commands, inbox) = channel();
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let shared = sink.clone();
        let volume = Arc::new(AtomicU32::new(0.7f32.to_bits()));
        let level = volume.clone();
        let _ = std::thread::Builder::new().name("audio".into()).spawn(move || {
            // Opened on first use, so a machine with no output only loses sound.
            let mut output = None;
            while let Ok(command) = inbox.recv() {
                if output.is_none() { output = OutputStream::try_default().ok(); }
                let Some((_, handle)) = &output else { continue };
                let Ok(next) = Sink::try_new(handle) else { continue };
                next.set_volume(f32::from_bits(level.load(Ordering::Relaxed)));
                match command {
                    Command::Play(path) => {
                        let opened = File::open(&path).ok().and_then(|f| Decoder::new(BufReader::new(f)).ok());
                        let Some(source) = opened else { continue };
                        next.append(source);
                    }
                    Command::Clip { pcm, looped, at } => {
                        let start = Clip::sample_index(&pcm, at);
                        next.append(Clip { pcm, next: start, looped });
                    }
                }
                if let Ok(mut slot) = shared.lock() { *slot = Some(next); }
            }
        });
        Audio { commands, sink, volume }
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
        self.with(|s| s.set_volume(volume));
    }

    pub fn play(&self, path: PathBuf) {
        self.stop();
        let _ = self.commands.send(Command::Play(path));
    }

    /// Plays `pcm` from `at` seconds, replacing whatever is playing.
    pub fn play_clip(&self, pcm: Arc<Pcm>, looped: bool, at: f32) {
        self.stop();
        let _ = self.commands.send(Command::Clip { pcm, looped, at });
    }

    pub fn stop(&self) {
        if let Ok(mut slot) = self.sink.lock() { *slot = None; }
    }

    fn with<T>(&self, f: impl FnOnce(&Sink) -> T) -> Option<T> {
        self.sink.lock().ok()?.as_ref().map(f)
    }

    /// Pauses or resumes; true when it's now playing.
    pub fn toggle(&self) -> bool {
        self.with(|s| {
            if s.is_paused() { s.play(); true } else { s.pause(); false }
        }).unwrap_or(false)
    }

    pub fn seek(&self, seconds: f32) {
        self.with(|s| {
            let _ = s.try_seek(Duration::from_secs_f32(seconds.max(0.0)));
        });
    }

    pub fn position(&self) -> Option<f32> {
        self.with(|s| s.get_pos().as_secs_f32())
    }

    pub fn is_playing(&self) -> bool {
        self.with(|s| !s.is_paused() && !s.empty()).unwrap_or(false)
    }

    /// Whether a sound finished (or nothing has been asked to play).
    pub fn is_finished(&self) -> bool {
        self.with(|s| s.empty()).unwrap_or(true)
    }
}
