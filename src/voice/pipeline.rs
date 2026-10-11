//! Device-free parts of the voice pipeline: packet format, resampling, Opus
//! encoding and the jitter buffer. Kept pure so they can be unit-tested
//! without a microphone or speakers.
//!
//! Every byte of an incoming packet comes from the peer and is untrusted:
//! packets are length-checked before they reach the Opus decoder.

use std::collections::BTreeMap;

/// All processing runs at 48 kHz mono.
pub const RATE: u32 = 48_000;
/// 10 ms: the frame size of the echo canceller.
pub const FRAME_10MS: usize = 480;
/// 20 ms: one Opus packet.
pub const FRAME_20MS: usize = 960;
/// Largest Opus packet the spec allows.
pub const MAX_OPUS_PACKET: usize = 1275;

const PACKET_VERSION: u8 = 1;
/// version (1) || call id (4) || sequence number (4)
const HEADER_LEN: usize = 9;

/// Constant bitrate: a variable bitrate would let the size of each encrypted
/// packet reveal something about what is being said.
const BITRATE: i32 = 32_000;
/// Packets buffered before playout starts (3 × 20 ms).
const PREBUFFER: usize = 3;
/// Above this many buffered packets (200 ms) the oldest are dropped, so a
/// network hiccup does not leave a permanent delay.
const MAX_BUFFERED: usize = 10;
/// Missing packets in a row before playout stops and re-buffers.
const MAX_CONCEALED: u32 = 5;

pub fn encode_packet(call_id: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(PACKET_VERSION);
    out.extend_from_slice(&call_id.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Returns `(call_id, seq, opus_payload)` for a well-formed packet.
pub fn parse_packet(data: &[u8]) -> Option<(u32, u32, &[u8])> {
    if data.len() <= HEADER_LEN || data.len() > HEADER_LEN + MAX_OPUS_PACKET {
        return None;
    }
    if data[0] != PACKET_VERSION {
        return None;
    }
    let call_id = u32::from_be_bytes(data[1..5].try_into().ok()?);
    let seq = u32::from_be_bytes(data[5..9].try_into().ok()?);
    Some((call_id, seq, &data[HEADER_LEN..]))
}

/// Streaming linear resampler for mono audio. Sound cards usually already run
/// at 48 kHz, in which case this is a plain copy.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Read position in `buf`, in input samples.
    pos: f64,
    buf: Vec<f32>,
}

impl Resampler {
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        Resampler {
            step: f64::from(from_hz) / f64::from(to_hz),
            pos: 0.0,
            buf: Vec::new(),
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        while self.pos + 1.0 < self.buf.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac);
            self.pos += self.step;
        }
        let used = (self.pos as usize).min(self.buf.len());
        self.buf.drain(..used);
        self.pos -= used as f64;
    }
}

/// Turns 10 ms frames of processed microphone audio into numbered packets.
pub struct VoiceEncoder {
    enc: opus::Encoder,
    call_id: u32,
    seq: u32,
    pending: Vec<f32>,
    out: Vec<u8>,
}

impl VoiceEncoder {
    pub fn new(call_id: u32) -> Result<Self, String> {
        let err = |e: opus::Error| format!("opus encoder: {e}");
        let mut enc =
            opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).map_err(err)?;
        enc.set_bitrate(opus::Bitrate::Bits(BITRATE)).map_err(err)?;
        enc.set_vbr(false).map_err(err)?;
        // Forward error correction lets the receiver rebuild a lost packet
        // from the next one.
        enc.set_inband_fec(true).map_err(err)?;
        enc.set_packet_loss_perc(10).map_err(err)?;
        Ok(VoiceEncoder {
            enc,
            call_id,
            seq: 0,
            pending: Vec::with_capacity(FRAME_20MS),
            out: vec![0; MAX_OPUS_PACKET],
        })
    }

    /// Adds samples; returns a packet each time 20 ms are complete.
    pub fn push(&mut self, samples: &[f32]) -> Result<Option<Vec<u8>>, String> {
        self.pending.extend_from_slice(samples);
        if self.pending.len() < FRAME_20MS {
            return Ok(None);
        }
        let len = self
            .enc
            .encode_float(&self.pending[..FRAME_20MS], &mut self.out)
            .map_err(|e| format!("opus encode: {e}"))?;
        self.pending.drain(..FRAME_20MS);
        let packet = encode_packet(self.call_id, self.seq, &self.out[..len]);
        self.seq = self.seq.wrapping_add(1);
        Ok(Some(packet))
    }
}

/// Jitter buffer and decoder: packets arrive late, early, twice or not at
/// all; playout takes exactly one 20 ms frame at a time.
pub struct Playout {
    dec: opus::Decoder,
    call_id: u32,
    packets: BTreeMap<u32, Vec<u8>>,
    next: Option<u32>,
    concealed: u32,
}

impl Playout {
    pub fn new(call_id: u32) -> Result<Self, String> {
        Ok(Playout {
            dec: opus::Decoder::new(RATE, opus::Channels::Mono)
                .map_err(|e| format!("opus decoder: {e}"))?,
            call_id,
            packets: BTreeMap::new(),
            next: None,
            concealed: 0,
        })
    }

    pub fn insert(&mut self, data: &[u8]) {
        let Some((call_id, seq, payload)) = parse_packet(data) else {
            return;
        };
        // Stale packets from an earlier call, and anything already played.
        if call_id != self.call_id || self.next.is_some_and(|n| seq < n) {
            return;
        }
        self.packets.insert(seq, payload.to_vec());
        while self.packets.len() > MAX_BUFFERED {
            self.packets.pop_first();
            if let Some((&first, _)) = self.packets.first_key_value()
                && self.next.is_some()
            {
                self.next = Some(first);
            }
        }
    }

    /// Fills `out` with the next 20 ms. Returns false while buffering
    /// (`out` is silence then).
    pub fn pop_frame(&mut self, out: &mut [f32; FRAME_20MS]) -> bool {
        let seq = match self.next {
            Some(n) => n,
            None => {
                if self.packets.len() < PREBUFFER {
                    out.fill(0.0);
                    return false;
                }
                *self.packets.keys().next().expect("non-empty")
            }
        };
        let decoded = match self.packets.remove(&seq) {
            Some(p) => {
                self.concealed = 0;
                self.dec.decode_float(&p, out, false)
            }
            None => {
                self.concealed += 1;
                if self.concealed > MAX_CONCEALED && self.packets.is_empty() {
                    // The other side went quiet or the network stalled.
                    self.next = None;
                    self.concealed = 0;
                    out.fill(0.0);
                    return false;
                }
                match self.packets.get(&seq.wrapping_add(1)) {
                    // Rebuild the lost packet from the FEC data in the next one.
                    Some(next) => self.dec.decode_float(next, out, true),
                    // Packet loss concealment.
                    None => self.dec.decode_float(&[], out, false),
                }
            }
        };
        if !matches!(decoded, Ok(n) if n == FRAME_20MS) {
            out.fill(0.0);
        }
        self.next = Some(seq.wrapping_add(1));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, start: usize) -> Vec<f32> {
        (start..start + n)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.3)
            .collect()
    }

    #[test]
    fn packet_roundtrip_and_rejects() {
        let p = encode_packet(7, 42, &[1, 2, 3]);
        assert_eq!(parse_packet(&p), Some((7, 42, &[1u8, 2, 3][..])));
        assert_eq!(parse_packet(&p[..HEADER_LEN]), None, "empty payload");
        assert_eq!(parse_packet(&[]), None);
        let mut bad = p.clone();
        bad[0] = 9;
        assert_eq!(parse_packet(&bad), None, "unknown version");
        let huge = encode_packet(7, 1, &vec![0; MAX_OPUS_PACKET + 1]);
        assert_eq!(parse_packet(&huge), None, "oversized payload");
    }

    #[test]
    fn resampler_ratio() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&vec![0.5; 441], &mut out);
        }
        let expected = 44_100 * 48_000 / 44_100;
        assert!(out.len().abs_diff(expected) <= 2, "{}", out.len());
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));

        let mut same = Resampler::new(48_000, 48_000);
        let mut out = Vec::new();
        same.process(&[0.1, 0.2], &mut out);
        assert_eq!(out, vec![0.1, 0.2]);
    }

    #[test]
    fn encode_decode_through_jitter_buffer() {
        let mut enc = VoiceEncoder::new(5).unwrap();
        let mut play = Playout::new(5).unwrap();
        let mut packets = Vec::new();
        for i in 0..20 {
            if let Some(p) = enc.push(&tone(FRAME_10MS, i * FRAME_10MS)).unwrap() {
                packets.push(p);
            }
        }
        assert_eq!(packets.len(), 10, "one packet per 20 ms");
        // Constant bitrate: every packet has the same size.
        assert!(packets.iter().all(|p| p.len() == packets[0].len()));

        // Delivered out of order and with a duplicate.
        packets.swap(1, 2);
        let dup = packets[3].clone();
        packets.push(dup);
        let mut frame = [0.0; FRAME_20MS];
        assert!(!play.pop_frame(&mut frame), "nothing buffered yet");
        for p in &packets {
            play.insert(p);
        }
        let mut energy = 0.0;
        for _ in 0..10 {
            assert!(play.pop_frame(&mut frame));
            energy += frame.iter().map(|s| s * s).sum::<f32>();
        }
        assert!(energy > 1.0, "decoded audio is not silence: {energy}");
    }

    #[test]
    fn loss_is_concealed_and_stale_calls_ignored() {
        let mut enc = VoiceEncoder::new(1).unwrap();
        let mut play = Playout::new(1).unwrap();
        let mut packets = Vec::new();
        for i in 0..12 {
            if let Some(p) = enc.push(&tone(FRAME_10MS, i * FRAME_10MS)).unwrap() {
                packets.push(p);
            }
        }
        packets.remove(3); // lost
        for p in &packets {
            play.insert(p);
        }
        play.insert(&encode_packet(99, 0, &[0xFF; 40])); // another call
        play.insert(&[0xFF; 3]); // garbage
        let mut frame = [0.0; FRAME_20MS];
        for _ in 0..6 {
            assert!(play.pop_frame(&mut frame), "lost packet is concealed");
        }
    }

    #[test]
    fn latency_is_bounded() {
        let mut enc = VoiceEncoder::new(2).unwrap();
        let mut play = Playout::new(2).unwrap();
        for i in 0..100 {
            if let Some(p) = enc.push(&tone(FRAME_10MS, i * FRAME_10MS)).unwrap() {
                play.insert(&p);
            }
        }
        assert!(play.packets.len() <= MAX_BUFFERED);
    }
}
