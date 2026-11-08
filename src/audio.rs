//! Playback on its own thread: the output stream can't leave the thread that made it, and
//! opening a file must never stall a frame.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{Decoder, OutputStream, Sink};

enum Command {
    Play(PathBuf),
}

#[derive(Clone)]
pub struct Audio {
    commands: Sender<Command>,
    sink: Arc<Mutex<Option<Sink>>>,
}

const VOLUME: f32 = 0.7;

impl Audio {
    pub fn new() -> Audio {
        let (commands, inbox) = channel();
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let shared = sink.clone();
        let _ = std::thread::Builder::new().name("audio".into()).spawn(move || {
            // Opened on first use, so a machine with no output only loses sound.
            let mut output = None;
            while let Ok(command) = inbox.recv() {
                let Command::Play(path) = command;
                if output.is_none() { output = OutputStream::try_default().ok(); }
                let Some((_, handle)) = &output else { continue };
                let Ok(next) = Sink::try_new(handle) else { continue };
                next.set_volume(VOLUME);
                let opened = File::open(&path).ok().and_then(|f| Decoder::new(BufReader::new(f)).ok());
                let Some(source) = opened else { continue };
                next.append(source);
                if let Ok(mut slot) = shared.lock() { *slot = Some(next); }
            }
        });
        Audio { commands, sink }
    }

    pub fn play(&self, path: PathBuf) {
        self.stop();
        let _ = self.commands.send(Command::Play(path));
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
