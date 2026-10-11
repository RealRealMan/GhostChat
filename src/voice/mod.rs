//! Voice calls (prototype).
//!
//! One call runs on its own thread for its whole lifetime: microphone →
//! echo cancellation / noise suppression (sonora) → Opus at a constant
//! bitrate → QUIC datagrams, and datagrams → jitter buffer → Opus → speakers.
//! The datagrams travel inside the existing iroh connection, so they get the
//! same end-to-end encryption as chat messages.
//!
//! The microphone is only open while a call is active: dropping [`Call`]
//! stops the thread, which closes the audio devices.

pub mod device;
pub mod pipeline;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use sonora::{AudioProcessing, Config, StreamConfig};

use pipeline::{FRAME_10MS, FRAME_20MS, Playout, RATE, Resampler, VoiceEncoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
    /// The default microphone and speakers.
    Devices,
    /// No sound card: the microphone is silence and playback is discarded.
    /// Lets tests run a full call on machines without audio hardware.
    Silent,
}

/// Sends one packet to the peer (an unreliable datagram).
pub type SendPacket = Box<dyn Fn(Vec<u8>) + Send>;
/// Called once if the call fails after it started (device unplugged, ...).
pub type OnFail = Box<dyn FnOnce(String) + Send>;

/// How long the audio devices may take to open.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Playback queued beyond this is dropped, so delay cannot build up.
const MAX_PLAYBACK_QUEUE: Duration = Duration::from_millis(300);

pub struct Call {
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}

impl Call {
    /// Opens the audio devices and starts streaming. Returns an error if the
    /// microphone or speakers cannot be opened.
    pub fn start(
        call_id: u32,
        backend: AudioBackend,
        send: SendPacket,
        incoming: mpsc::Receiver<Vec<u8>>,
        on_fail: OnFail,
    ) -> Result<Call, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (thread_stop, thread_muted) = (stop.clone(), muted.clone());
        thread::Builder::new()
            .name("ghostchat-voice".into())
            .spawn(move || {
                let ctx = Ctx {
                    call_id,
                    send,
                    incoming,
                    stop: thread_stop,
                    muted: thread_muted,
                };
                // Setup errors go back to `start`; later ones to `on_fail`.
                if let Err(e) = run(ctx, backend, ready_tx) {
                    on_fail(e);
                }
            })
            .map_err(|e| format!("could not start the audio thread: {e}"))?;
        let call = Call { stop, muted };
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(call),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the audio devices did not start".into()),
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        // The thread notices within one capture timeout and closes the devices.
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Ctx {
    call_id: u32,
    send: SendPacket,
    incoming: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}

enum Io {
    Devices {
        input: Box<device::Input>,
        output: Box<device::Output>,
        errors: mpsc::Receiver<String>,
    },
    Silent {
        next: Instant,
    },
}

impl Io {
    fn open(backend: AudioBackend) -> Result<Io, String> {
        match backend {
            AudioBackend::Silent => Ok(Io::Silent {
                next: Instant::now(),
            }),
            AudioBackend::Devices => {
                let (tx, errors) = mpsc::channel();
                Ok(Io::Devices {
                    input: Box::new(device::open_input(tx.clone())?),
                    output: Box::new(device::open_output(tx)?),
                    errors,
                })
            }
        }
    }

    fn rates(&self) -> (u32, u32) {
        match self {
            Io::Devices { input, output, .. } => (input.rate, output.rate),
            Io::Silent { .. } => (RATE, RATE),
        }
    }

    /// The next chunk of microphone audio, or `None` if nothing arrived in time.
    fn capture(&mut self) -> Result<Option<Vec<f32>>, String> {
        const WAIT: Duration = Duration::from_millis(100);
        match self {
            Io::Devices { input, errors, .. } => {
                if let Ok(e) = errors.try_recv() {
                    return Err(e);
                }
                match input.chunks.recv_timeout(WAIT) {
                    Ok(chunk) => Ok(Some(chunk)),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Err("the microphone stopped".into())
                    }
                }
            }
            Io::Silent { next } => {
                *next += Duration::from_millis(10);
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    thread::sleep(wait);
                }
                Ok(Some(vec![0.0; FRAME_10MS]))
            }
        }
    }

    fn play(&mut self, samples: &[f32]) {
        if let Io::Devices { output, .. } = self {
            let max = (output.rate as f32 * MAX_PLAYBACK_QUEUE.as_secs_f32()) as usize;
            let mut q = output.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.extend(samples);
            let excess = q.len().saturating_sub(max);
            q.drain(..excess);
        }
    }
}

fn audio_processing() -> AudioProcessing {
    let stream = StreamConfig::new(RATE, 1);
    AudioProcessing::builder()
        .config(Config {
            echo_canceller: Some(Default::default()),
            noise_suppression: Some(Default::default()),
            high_pass_filter: Some(Default::default()),
            gain_controller2: Some(Default::default()),
            ..Default::default()
        })
        .capture_config(stream)
        .render_config(stream)
        .build()
}

fn run(
    ctx: Ctx,
    backend: AudioBackend,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let setup = (|| {
        let io = Io::open(backend)?;
        let enc = VoiceEncoder::new(ctx.call_id)?;
        let playout = Playout::new(ctx.call_id)?;
        Ok((io, enc, playout))
    })();
    let (mut io, mut enc, mut playout) = match setup {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return Ok(());
        }
    };
    let mut apm = audio_processing();
    let (in_rate, out_rate) = io.rates();
    let mut to_48k = Resampler::new(in_rate, RATE);
    let mut from_48k = Resampler::new(RATE, out_rate);
    let apm_err = |e: sonora::Error| format!("audio processing: {e:?}");

    let mut captured = Vec::new();
    let mut frames: u64 = 0;
    let mut play = [0.0f32; FRAME_20MS];
    let mut resampled = Vec::new();
    let mut scratch = [0.0f32; FRAME_10MS];
    let mut processed = [0.0f32; FRAME_10MS];

    // The microphone is the clock: every 10 ms of captured audio drives one
    // echo-canceller step, and every 20 ms one packet out and one frame of
    // playback.
    while !ctx.stop.load(Ordering::Relaxed) {
        let Some(chunk) = io.capture()? else {
            continue;
        };
        while let Ok(packet) = ctx.incoming.try_recv() {
            playout.insert(&packet);
        }
        to_48k.process(&chunk, &mut captured);
        while captured.len() >= FRAME_10MS {
            if frames.is_multiple_of(2) {
                playout.pop_frame(&mut play);
                // Tell the echo canceller what the speakers are about to play.
                for half in play.chunks(FRAME_10MS) {
                    apm.process_render_f32(&[half], &mut [&mut scratch[..]])
                        .map_err(apm_err)?;
                }
                resampled.clear();
                from_48k.process(&play, &mut resampled);
                io.play(&resampled);
            }
            frames += 1;
            let frame: Vec<f32> = captured.drain(..FRAME_10MS).collect();
            apm.process_capture_f32(&[&frame], &mut [&mut processed[..]])
                .map_err(apm_err)?;
            // Muted still sends (silent) packets: the traffic pattern does not
            // reveal when someone mutes.
            if ctx.muted.load(Ordering::Relaxed) {
                processed.fill(0.0);
            }
            if let Some(packet) = enc.push(&processed)? {
                (ctx.send)(packet);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a microphone and speakers: `cargo test real_devices -- --ignored`.
    /// Nothing is sent anywhere; incoming audio is empty, so the speakers
    /// only play silence.
    #[test]
    #[ignore = "needs a microphone and speakers"]
    fn real_devices_stream_packets() {
        let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = sent.clone();
        let (_tx, rx) = mpsc::channel();
        let call = Call::start(
            1,
            AudioBackend::Devices,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            rx,
            Box::new(|e| panic!("call failed: {e}")),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(1000));
        drop(call);
        let n = sent.load(Ordering::Relaxed);
        assert!(n >= 30, "about 50 packets per second, got {n}");
    }

    /// Two silent calls wired to each other exchange packets.
    #[test]
    fn silent_calls_exchange_packets() {
        let (a_tx, a_rx) = mpsc::channel::<Vec<u8>>();
        let (b_tx, b_rx) = mpsc::channel::<Vec<u8>>();
        let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = sent.clone();
        let a = Call::start(
            1,
            AudioBackend::Silent,
            Box::new(move |p| {
                counter.fetch_add(1, Ordering::Relaxed);
                let _ = b_tx.send(p);
            }),
            a_rx,
            Box::new(|e| panic!("call a failed: {e}")),
        )
        .unwrap();
        let b = Call::start(
            1,
            AudioBackend::Silent,
            Box::new(move |p| {
                let _ = a_tx.send(p);
            }),
            b_rx,
            Box::new(|e| panic!("call b failed: {e}")),
        )
        .unwrap();
        a.set_muted(true);
        thread::sleep(Duration::from_millis(300));
        let n = sent.load(Ordering::Relaxed);
        assert!(n >= 8, "about one packet per 20 ms, got {n}");
        drop((a, b));
    }
}
