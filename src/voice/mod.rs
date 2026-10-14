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
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use sonora::{AudioProcessing, Config, StreamConfig};

use pipeline::{FRAME_10MS, FRAME_20MS, Playout, RATE, Resampler, VoiceEncoder};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioBackend {
    /// Real microphone and speakers.
    Devices(DeviceChoice),
    /// No sound card: the microphone is silence and playback is discarded.
    /// Lets tests run a full call on machines without audio hardware.
    Silent,
}

/// Which sound devices to use, by name. `None` = the system default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceChoice {
    pub input: Option<String>,
    pub output: Option<String>,
}

/// Live speech levels for the UI (0.0 = silence, 1.0 = loud), written by
/// the audio thread and read by the UI without locking.
#[derive(Debug, Clone, Default)]
pub struct Meter(Arc<[AtomicU32; 4]>);

impl Meter {
    /// Your own microphone, after echo cancellation (0 while muted).
    pub fn mic(&self) -> f32 {
        f32::from_bits(self.0[0].load(Ordering::Relaxed))
    }

    /// The other person's voice.
    pub fn peer(&self) -> f32 {
        f32::from_bits(self.0[1].load(Ordering::Relaxed))
    }

    /// The microphone delivers no sound at all (muted or turned down to
    /// zero in the operating system).
    pub fn no_signal(&self) -> bool {
        self.0[2].load(Ordering::Relaxed) != 0
    }

    fn set_no_signal(&self, yes: bool) {
        self.0[2].store(u32::from(yes), Ordering::Relaxed);
    }

    /// The echo guard is lowering the microphone right now.
    pub fn echo_guard(&self) -> bool {
        self.0[3].load(Ordering::Relaxed) != 0
    }

    fn set_echo_guard(&self, on: bool) {
        self.0[3].store(u32::from(on), Ordering::Relaxed);
    }

    fn set(&self, i: usize, samples: &[f32]) {
        self.0[i].store(level(samples).to_bits(), Ordering::Relaxed);
    }

    fn reset(&self) {
        for v in self.0.iter() {
            v.store(0f32.to_bits(), Ordering::Relaxed);
        }
    }
}

/// RMS level mapped from -60..0 dBFS to 0..1.
fn level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms <= 0.0 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Sends one packet to the peer (an unreliable datagram).
pub type SendPacket = Box<dyn Fn(Vec<u8>) + Send>;
/// Called once if the call fails after it started (device unplugged, ...).
pub type OnFail = Box<dyn FnOnce(String) + Send>;

/// How long the audio devices may take to open.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Playback queued beyond this is dropped, so delay cannot build up.
const MAX_PLAYBACK_QUEUE: Duration = Duration::from_millis(300);
/// Rough capture + output buffering inside the sound card drivers, added to
/// the known playback queue for the echo canceller's delay hint.
const DEVICE_LATENCY_MS: i32 = 40;
/// Re-opening the devices after Windows reset a stream (mute toggled,
/// format changed, device replugged).
const REOPEN_ATTEMPTS: u32 = 10;
const REOPEN_DELAY: Duration = Duration::from_millis(300);
/// A working microphone always picks up some noise; below this peak level
/// (-80 dBFS) for `NO_SIGNAL_AFTER` it is muted or turned down to zero.
const NO_SIGNAL_PEAK: f32 = 1e-4;
const NO_SIGNAL_AFTER: Duration = Duration::from_secs(3);
/// Echo guard: while the other person's voice is playing and echo is likely,
/// the microphone is lowered by 30 dB so they never hear themselves.
const GUARD_GAIN: f32 = 0.03;
/// Their voice counts as playing above this level (about -51 dBFS) ...
const FAR_ACTIVE_LEVEL: f32 = 0.15;
/// ... and for this long afterwards: the echo tail of a room.
const FAR_HANGOVER: Duration = Duration::from_millis(300);
/// The echo canceller needs a few seconds to learn the room.
const GUARD_CONVERGE: Duration = Duration::from_secs(3);
/// Residual echo likelihood (0..1, from the echo detector) treated as echo.
const ECHO_LIKELY: f64 = 0.3;
/// Gain recovery per 10 ms frame once the guard releases (full in 100 ms).
const GUARD_RELEASE_STEP: f32 = 0.1;

pub struct Call {
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}

impl Call {
    /// Opens the audio devices and starts streaming. Returns an error if the
    /// microphone or speakers cannot be opened.
    pub fn start(
        call_id: u32,
        backend: &AudioBackend,
        meter: Meter,
        send: SendPacket,
        incoming: mpsc::Receiver<Vec<u8>>,
        on_fail: OnFail,
    ) -> Result<Call, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (thread_stop, thread_muted) = (stop.clone(), muted.clone());
        let backend = backend.clone();
        thread::Builder::new()
            .name("ghostchat-voice".into())
            .spawn(move || {
                let ctx = Ctx {
                    call_id,
                    send,
                    incoming,
                    stop: thread_stop,
                    muted: thread_muted,
                    meter: meter.clone(),
                };
                // Setup errors go back to `start`; later ones to `on_fail`.
                let result = run(ctx, backend, ready_tx);
                meter.reset();
                if let Err(e) = result {
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
    meter: Meter,
}

enum Io {
    Devices {
        input: Box<device::Input>,
        output: Box<device::Output>,
        errors: mpsc::Receiver<device::DeviceError>,
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
            AudioBackend::Devices(choice) => {
                let (tx, errors) = mpsc::channel();
                Ok(Io::Devices {
                    input: Box::new(device::open_input(choice.input.as_deref(), tx.clone())?),
                    output: Box::new(device::open_output(choice.output.as_deref(), tx)?),
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

    /// The next chunk of microphone audio, or `None` if nothing arrived in
    /// time. `Err` means the devices stopped working and must be re-opened.
    fn capture(&mut self) -> Result<Option<Vec<f32>>, String> {
        const WAIT: Duration = Duration::from_millis(100);
        match self {
            Io::Devices { input, errors, .. } => {
                // Dropouts only cost a few milliseconds of sound.
                while let Ok(e) = errors.try_recv() {
                    if let device::DeviceError::Lost(reason) = e {
                        return Err(reason);
                    }
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

    /// Audio queued for the speakers but not played yet.
    fn playback_delay_ms(&self) -> i32 {
        match self {
            Io::Devices { output, .. } => {
                let queued = output.queue.lock().unwrap_or_else(|p| p.into_inner()).len();
                (queued as u64 * 1000 / u64::from(output.rate.max(1))) as i32
            }
            Io::Silent { .. } => 0,
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
        // Provides `residual_echo_likelihood` for the echo guard.
        .echo_detector(true)
        .build()
}

/// Opens the devices again after they stopped. Gives up (ending the call)
/// only if they stay unavailable.
fn reopen(backend: &AudioBackend, stop: &AtomicBool, reason: String) -> Result<Io, String> {
    for _ in 0..REOPEN_ATTEMPTS {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(REOPEN_DELAY);
        if let Ok(io) = Io::open(backend.clone()) {
            return Ok(io);
        }
    }
    Err(reason)
}

/// Last line of defence against echo, after the echo canceller.
struct EchoGuard {
    started: Instant,
    far_until: Option<Instant>,
    gain: f32,
}

impl EchoGuard {
    fn new(now: Instant) -> Self {
        EchoGuard {
            started: now,
            far_until: None,
            gain: 1.0,
        }
    }

    /// Called with each frame sent to the speakers.
    fn far_end(&mut self, level: f32, now: Instant) {
        if level > FAR_ACTIVE_LEVEL {
            self.far_until = Some(now + FAR_HANGOVER);
        }
    }

    /// Lowers `frame` when it may carry echo. Returns whether the guard is on.
    fn apply(&mut self, frame: &mut [f32], echo_likelihood: f64, now: Instant) -> bool {
        let far = self.far_until.is_some_and(|t| now < t);
        let learning = now.duration_since(self.started) < GUARD_CONVERGE;
        let on = far && (learning || echo_likelihood > ECHO_LIKELY);
        let from = self.gain;
        // Drop at once; come back gradually so there is no click.
        self.gain = if on {
            GUARD_GAIN
        } else {
            (self.gain + GUARD_RELEASE_STEP).min(1.0)
        };
        let n = frame.len().max(1) as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            *s *= from + (self.gain - from) * (i + 1) as f32 / n;
        }
        on
    }
}

/// Notices a microphone that delivers no sound at all.
#[derive(Default)]
struct SignalWatch {
    quiet_since: Option<Instant>,
}

impl SignalWatch {
    /// Returns true once the microphone has been dead silent for a while.
    fn update(&mut self, raw: &[f32], now: Instant) -> bool {
        let peak = raw.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak >= NO_SIGNAL_PEAK {
            self.quiet_since = None;
            return false;
        }
        let since = *self.quiet_since.get_or_insert(now);
        now.duration_since(since) >= NO_SIGNAL_AFTER
    }
}

fn run(
    ctx: Ctx,
    backend: AudioBackend,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let setup = (|| {
        let io = Io::open(backend.clone())?;
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
    let mut watch = SignalWatch::default();
    let mut guard = EchoGuard::new(Instant::now());

    // The microphone is the clock: every 10 ms of captured audio drives one
    // echo-canceller step, and every 20 ms one packet out and one frame of
    // playback.
    while !ctx.stop.load(Ordering::Relaxed) {
        let chunk = match io.capture() {
            Ok(Some(chunk)) => chunk,
            Ok(None) => continue,
            Err(reason) => {
                // Close the old streams before opening new ones.
                drop(std::mem::replace(
                    &mut io,
                    Io::Silent {
                        next: Instant::now(),
                    },
                ));
                io = reopen(&backend, &ctx.stop, reason)?;
                let (in_rate, out_rate) = io.rates();
                to_48k = Resampler::new(in_rate, RATE);
                from_48k = Resampler::new(RATE, out_rate);
                captured.clear();
                continue;
            }
        };
        while let Ok(packet) = ctx.incoming.try_recv() {
            playout.insert(&packet);
        }
        to_48k.process(&chunk, &mut captured);
        while captured.len() >= FRAME_10MS {
            if frames.is_multiple_of(2) {
                playout.pop_frame(&mut play);
                ctx.meter.set(1, &play);
                guard.far_end(level(&play), Instant::now());
                // Tell the echo canceller what the speakers are about to play.
                for half in play.chunks(FRAME_10MS) {
                    apm.process_render_f32(&[half], &mut [&mut scratch[..]])
                        .map_err(apm_err)?;
                }
                resampled.clear();
                from_48k.process(&play, &mut resampled);
                io.play(&resampled);
                // Echo arrives at the microphone this much later than the
                // render frame was analysed. A clamped hint is still applied,
                // so its error is not a failure.
                let delay = (io.playback_delay_ms() + DEVICE_LATENCY_MS).clamp(0, 500);
                let _ = apm.set_stream_delay_ms(delay);
            }
            frames += 1;
            let frame: Vec<f32> = captured.drain(..FRAME_10MS).collect();
            ctx.meter
                .set_no_signal(watch.update(&frame, Instant::now()));
            apm.process_capture_f32(&[&frame], &mut [&mut processed[..]])
                .map_err(apm_err)?;
            let echo = apm
                .statistics()
                .residual_echo_likelihood_recent_max
                .unwrap_or(0.0);
            ctx.meter
                .set_echo_guard(guard.apply(&mut processed, echo, Instant::now()));
            // Muted still sends (silent) packets: the traffic pattern does not
            // reveal when someone mutes.
            if ctx.muted.load(Ordering::Relaxed) {
                processed.fill(0.0);
            }
            ctx.meter.set(0, &processed);
            if let Some(packet) = enc.push(&processed)? {
                (ctx.send)(packet);
            }
        }
    }
    Ok(())
}

/// How long the voice test records before playing it back.
pub const TEST_RECORD: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPhase {
    Recording,
    Playing,
    Done,
    Failed,
}

/// A local voice test: records a few seconds from the microphone through the
/// same processing as a call (noise suppression, Opus), then plays it back.
/// Nothing is sent over the network and nothing is stored on disk.
pub struct VoiceTest {
    stop: Arc<AtomicBool>,
    state: Arc<TestState>,
}

#[derive(Default)]
struct TestState {
    phase: AtomicU8,
    /// Loudest microphone level while recording (f32 bits).
    peak: AtomicU32,
    error: Mutex<Option<String>>,
}

impl TestState {
    fn set_phase(&self, p: TestPhase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
}

impl VoiceTest {
    /// Starts at once; a device problem shows up as [`TestPhase::Failed`].
    pub fn start(backend: &AudioBackend, meter: Meter) -> VoiceTest {
        Self::start_for(backend, meter, TEST_RECORD)
    }

    fn start_for(backend: &AudioBackend, meter: Meter, record: Duration) -> VoiceTest {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(TestState::default());
        let (t_stop, t_state, backend) = (stop.clone(), state.clone(), backend.clone());
        let spawned = thread::Builder::new()
            .name("ghostchat-voicetest".into())
            .spawn(move || {
                if let Err(e) = run_test(&backend, &meter, &t_state, &t_stop, record) {
                    *t_state.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                    t_state.set_phase(TestPhase::Failed);
                }
                meter.reset();
            });
        if let Err(e) = spawned {
            *state.error.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(format!("could not start the audio thread: {e}"));
            state.set_phase(TestPhase::Failed);
        }
        VoiceTest { stop, state }
    }

    pub fn phase(&self) -> TestPhase {
        match self.state.phase.load(Ordering::Relaxed) {
            0 => TestPhase::Recording,
            1 => TestPhase::Playing,
            2 => TestPhase::Done,
            _ => TestPhase::Failed,
        }
    }

    /// Loudest microphone level while recording (0..1).
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.state.peak.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.state
            .error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Drop for VoiceTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run_test(
    backend: &AudioBackend,
    meter: &Meter,
    state: &TestState,
    stop: &AtomicBool,
    record: Duration,
) -> Result<(), String> {
    let mut io = Io::open(backend.clone())?;
    let mut apm = audio_processing();
    let mut enc = VoiceEncoder::new(0)?;
    let mut playout = Playout::new(0)?;
    let (in_rate, out_rate) = io.rates();
    let mut to_48k = Resampler::new(in_rate, RATE);
    let mut from_48k = Resampler::new(RATE, out_rate);
    let apm_err = |e: sonora::Error| format!("audio processing: {e:?}");
    let record_frames = (record.as_millis() / 10) as usize;

    let mut captured = Vec::new();
    let mut processed = [0.0f32; FRAME_10MS];
    let mut packets = Vec::new();
    let mut watch = SignalWatch::default();
    let mut frames = 0usize;
    let mut peak = 0.0f32;

    // Record.
    while frames < record_frames {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some(chunk) = io.capture()? else {
            continue;
        };
        to_48k.process(&chunk, &mut captured);
        while captured.len() >= FRAME_10MS && frames < record_frames {
            let frame: Vec<f32> = captured.drain(..FRAME_10MS).collect();
            meter.set_no_signal(watch.update(&frame, Instant::now()));
            apm.process_capture_f32(&[&frame], &mut [&mut processed[..]])
                .map_err(apm_err)?;
            meter.set(0, &processed);
            peak = peak.max(level(&processed));
            state.peak.store(peak.to_bits(), Ordering::Relaxed);
            if let Some(p) = enc.push(&processed)? {
                packets.push(p);
            }
            frames += 1;
        }
    }
    meter.set(0, &[]);
    meter.set_no_signal(false);

    // Play back, paced by the microphone clock like a call. The jitter
    // buffer holds a few packets, so they are fed one per 20 ms.
    state.set_phase(TestPhase::Playing);
    let mut next = packets.into_iter();
    let mut play = [0.0f32; FRAME_20MS];
    let mut resampled = Vec::new();
    let mut remaining = record_frames / 2 + 4; // + jitter buffer prefill
    captured.clear();
    while remaining > 0 {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some(chunk) = io.capture()? else {
            continue;
        };
        to_48k.process(&chunk, &mut captured);
        while captured.len() >= FRAME_20MS && remaining > 0 {
            captured.drain(..FRAME_20MS);
            if let Some(p) = next.next() {
                playout.insert(&p);
            }
            playout.pop_frame(&mut play);
            meter.set(1, &play);
            resampled.clear();
            from_48k.process(&play, &mut resampled);
            io.play(&resampled);
            remaining -= 1;
        }
    }
    // Let the speakers finish what is queued.
    while io.playback_delay_ms() > 0 && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20));
    }
    state.set_phase(TestPhase::Done);
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
        let meter = Meter::default();
        let call = Call::start(
            1,
            &AudioBackend::Devices(DeviceChoice::default()),
            meter.clone(),
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            rx,
            Box::new(|e| panic!("call failed: {e}")),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(1000));
        eprintln!("mic level while running: {:.2}", meter.mic());
        drop(call);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(meter.mic(), 0.0, "levels reset when the call ends");
        let n = sent.load(Ordering::Relaxed);
        assert!(n >= 30, "about 50 packets per second, got {n}");
    }

    #[test]
    fn level_scale() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0.0; 480]), 0.0);
        assert!(level(&[0.001; 480]) < 0.05, "-60 dBFS is near silent");
        assert!(
            (level(&[0.5; 480]) - 0.9).abs() < 0.01,
            "-6 dBFS maps to 0.9"
        );
        assert_eq!(level(&[2.0; 480]), 1.0, "clamped");
    }

    #[test]
    fn echo_guard_lowers_the_mic_only_when_echo_is_possible() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let peak = |f: &[f32]| f.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let mut g = EchoGuard::new(t0);

        // Nobody is talking on the other side: never lowered, even early.
        let mut f = [0.5f32; FRAME_10MS];
        assert!(!g.apply(&mut f, 1.0, at(10)));
        assert_eq!(peak(&f), 0.5);

        // Their voice plays while the echo canceller is still learning.
        g.far_end(0.8, at(20));
        let mut f = [0.5f32; FRAME_10MS];
        assert!(g.apply(&mut f, 0.0, at(30)));
        assert!(
            (f[FRAME_10MS - 1] - 0.5 * GUARD_GAIN).abs() < 1e-6,
            "lowered by 30 dB"
        );

        // Later, with headphones (no echo detected): your voice goes through
        // while theirs plays, after a short click-free ramp.
        g.far_end(0.8, at(5_000));
        for i in 0..10 {
            let mut f = [0.5f32; FRAME_10MS];
            assert!(!g.apply(&mut f, 0.0, at(5_000 + i)));
        }
        let mut f = [0.5f32; FRAME_10MS];
        g.apply(&mut f, 0.0, at(5_020));
        assert_eq!(peak(&f), 0.5);

        // Speakers with echo detected: lowered again.
        let mut f = [0.5f32; FRAME_10MS];
        assert!(g.apply(&mut f, 0.9, at(5_030)));

        // Their voice stopped longer than the echo tail ago: released.
        let mut f = [0.5f32; FRAME_10MS];
        assert!(!g.apply(&mut f, 0.9, at(5_000 + 400)));
    }

    /// Needs a microphone and speakers: `cargo test real_devices -- --ignored`.
    /// Plays one second of the microphone back on the speakers.
    #[test]
    #[ignore = "needs a microphone and speakers"]
    fn real_devices_voice_test() {
        let t = VoiceTest::start_for(
            &AudioBackend::Devices(DeviceChoice::default()),
            Meter::default(),
            Duration::from_secs(1),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while t.phase() != TestPhase::Done {
            assert_ne!(t.phase(), TestPhase::Failed, "{:?}", t.error());
            assert!(Instant::now() < deadline, "voice test did not finish");
            thread::sleep(Duration::from_millis(20));
        }
        eprintln!("voice test peak level: {:.2}", t.peak());
    }

    #[test]
    fn voice_test_records_then_plays_back() {
        let meter = Meter::default();
        let t = VoiceTest::start_for(
            &AudioBackend::Silent,
            meter.clone(),
            Duration::from_millis(300),
        );
        assert_eq!(t.phase(), TestPhase::Recording);
        let mut saw_playing = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while t.phase() != TestPhase::Done {
            assert_ne!(t.phase(), TestPhase::Failed, "{:?}", t.error());
            assert!(Instant::now() < deadline, "voice test did not finish");
            saw_playing |= t.phase() == TestPhase::Playing;
            thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_playing);
        assert_eq!(t.peak(), 0.0, "silence records silence");
    }

    #[test]
    fn dead_microphone_is_noticed() {
        let mut w = SignalWatch::default();
        let t0 = Instant::now();
        let silent = [0.0f32; FRAME_10MS];
        let noise = [0.001f32; FRAME_10MS]; // -60 dBFS: any real microphone
        assert!(!w.update(&silent, t0));
        assert!(!w.update(&silent, t0 + Duration::from_secs(2)));
        assert!(w.update(&silent, t0 + Duration::from_secs(3)));
        assert!(
            !w.update(&noise, t0 + Duration::from_secs(4)),
            "signal resets it"
        );
        assert!(!w.update(&silent, t0 + Duration::from_secs(5)));

        let m = Meter::default();
        m.set_no_signal(true);
        assert!(m.no_signal());
        m.reset();
        assert!(!m.no_signal(), "reset when the call ends");
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
            &AudioBackend::Silent,
            Meter::default(),
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
            &AudioBackend::Silent,
            Meter::default(),
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
