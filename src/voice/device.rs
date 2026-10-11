//! Microphone and speaker access through cpal. Converts whatever the sound
//! card uses (channels, sample format) to mono `f32` and back.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
};

use cpal::{
    SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

/// Errors reported by the audio callbacks (device unplugged, ...).
pub type ErrorTx = mpsc::Sender<String>;

pub struct Input {
    _stream: cpal::Stream,
    pub rate: u32,
    /// Mono chunks as delivered by the sound card.
    pub chunks: mpsc::Receiver<Vec<f32>>,
}

pub struct Output {
    _stream: cpal::Stream,
    pub rate: u32,
    /// Mono samples waiting to be played; silence when empty.
    pub queue: Arc<Mutex<VecDeque<f32>>>,
}

pub fn open_input(name: Option<&str>, errors: ErrorTx) -> Result<Input, String> {
    let host = cpal::default_host();
    let device = match name {
        Some(n) => find(host.input_devices(), n)
            .ok_or_else(|| format!("microphone \"{n}\" not found — /mic to choose another"))?,
        None => host.default_input_device().ok_or("no microphone found")?,
    };
    let supported = device
        .default_input_config()
        .map_err(|e| format!("microphone: {e}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let (tx, chunks) = mpsc::channel();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(&device, config, channels, tx, errors, |s| s),
        SampleFormat::I16 => build_input::<i16>(&device, config, channels, tx, errors, |s| {
            f32::from(s) / f32::from(i16::MAX)
        }),
        other => return Err(format!("unsupported microphone sample format {other:?}")),
    }?;
    stream.play().map_err(|e| format!("microphone: {e}"))?;
    Ok(Input {
        _stream: stream,
        rate,
        chunks,
    })
}

fn build_input<T: SizedSample + Copy + Send + 'static>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    tx: mpsc::Sender<Vec<f32>>,
    errors: ErrorTx,
    to_f32: fn(T) -> f32,
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                // Mix all channels down to mono.
                let mono = data
                    .chunks(channels)
                    .map(|f| f.iter().map(|&s| to_f32(s)).sum::<f32>() / channels as f32)
                    .collect();
                let _ = tx.send(mono);
            },
            move |e| {
                let _ = errors.send(format!("microphone: {e}"));
            },
            None,
        )
        .map_err(|e| format!("microphone: {e}"))
}

pub fn open_output(name: Option<&str>, errors: ErrorTx) -> Result<Output, String> {
    let host = cpal::default_host();
    let device = match name {
        Some(n) => find(host.output_devices(), n)
            .ok_or_else(|| format!("speakers \"{n}\" not found — /speaker to choose another"))?,
        None => host.default_output_device().ok_or("no speakers found")?,
    };
    let supported = device
        .default_output_config()
        .map_err(|e| format!("speakers: {e}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let stream = match format {
        SampleFormat::F32 => {
            build_output::<f32>(&device, config, channels, queue.clone(), errors, |s| s)
        }
        SampleFormat::I16 => {
            build_output::<i16>(&device, config, channels, queue.clone(), errors, |s| {
                (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
            })
        }
        other => return Err(format!("unsupported speaker sample format {other:?}")),
    }?;
    stream.play().map_err(|e| format!("speakers: {e}"))?;
    Ok(Output {
        _stream: stream,
        rate,
        queue,
    })
}

fn build_output<T: SizedSample + Copy + Send + 'static>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
    errors: ErrorTx,
    from_f32: fn(f32) -> T,
) -> Result<cpal::Stream, String> {
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
                for frame in data.chunks_mut(channels) {
                    let s = from_f32(q.pop_front().unwrap_or(0.0));
                    frame.fill(s);
                }
            },
            move |e| {
                let _ = errors.send(format!("speakers: {e}"));
            },
            None,
        )
        .map_err(|e| format!("speakers: {e}"))
}

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

fn find<I: Iterator<Item = cpal::Device>, E>(
    devices: Result<I, E>,
    name: &str,
) -> Option<cpal::Device> {
    devices
        .ok()?
        .find(|d| device_name(d).as_deref() == Some(name))
}

/// Names of the available microphones and speakers.
///
/// Runs on a short-lived thread: on Windows, listing devices initializes COM,
/// which must not change the threading mode of the UI thread.
pub fn list() -> Result<(Vec<String>, Vec<String>), String> {
    std::thread::spawn(|| {
        let host = cpal::default_host();
        let names =
            |devices: Result<Vec<cpal::Device>, cpal::Error>| -> Result<Vec<String>, String> {
                Ok(devices
                    .map_err(|e| e.to_string())?
                    .iter()
                    .filter_map(device_name)
                    .collect())
            };
        let inputs = names(host.input_devices().map(|d| d.collect()))?;
        let outputs = names(host.output_devices().map(|d| d.collect()))?;
        Ok((inputs, outputs))
    })
    .join()
    .map_err(|_| "listing audio devices failed".to_string())?
}
