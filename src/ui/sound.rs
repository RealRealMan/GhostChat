//! Short alert chirp for new messages while the window is in the background.
//!
//! The tone is generated, not loaded from a file. The audio device is opened
//! on its own thread, lazily on the first chirp: opening it on the UI thread
//! can clash with the window's COM / drag-and-drop setup on Windows and would
//! block the UI if the device is slow.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use rodio::{DeviceSinkBuilder, Source, source::SineWave};

const VOLUME: f32 = 0.15;
/// Two rising tones: (frequency Hz, start ms, length ms).
const TONES: [(f32, u64, u64); 2] = [(1320.0, 0, 70), (1760.0, 90, 90)];

#[derive(Default)]
pub struct Beeper {
    tx: Option<mpsc::Sender<()>>,
    /// Set by the audio thread when no output device could be opened.
    error: Arc<Mutex<Option<String>>>,
}

impl Beeper {
    pub fn chirp(&mut self) {
        let tx = self.tx.get_or_insert_with(|| spawn(self.error.clone()));
        // A send error means the audio thread already gave up; the reason is
        // reported once through `take_error`.
        let _ = tx.send(());
    }

    /// The reason sound is unavailable, returned once.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok()?.take()
    }
}

fn spawn(error: Arc<Mutex<Option<String>>>) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let thread_error = error.clone();
    let started = thread::Builder::new()
        .name("ghostchat-sound".into())
        .spawn(move || {
            let mut sink = match DeviceSinkBuilder::open_default_sink() {
                Ok(s) => s,
                Err(e) => {
                    if let Ok(mut slot) = thread_error.lock() {
                        *slot = Some(e.to_string());
                    }
                    return;
                }
            };
            sink.log_on_drop(false);
            // Ends when the Beeper (and with it the sender) is dropped.
            while rx.recv().is_ok() {
                for (freq, start, len) in TONES {
                    sink.mixer().add(
                        SineWave::new(freq)
                            .take_duration(Duration::from_millis(len))
                            .delay(Duration::from_millis(start))
                            .amplify(VOLUME),
                    );
                }
            }
        });
    if let Err(e) = started
        && let Ok(mut slot) = error.lock()
    {
        *slot = Some(format!("could not start the sound thread: {e}"));
    }
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a real audio output device: `cargo test chirp -- --ignored`.
    #[test]
    #[ignore = "needs an audio output device"]
    fn chirp_on_real_device() {
        let mut b = Beeper::default();
        b.chirp();
        thread::sleep(Duration::from_millis(500));
        assert_eq!(b.take_error(), None);
    }
}
