//! Wire format (ARCHITECTURE §7): `u32 BE length || postcard(frame)`.
//!
//! Every byte read here comes from a peer and is untrusted: lengths are
//! checked before allocating, and decoding failures are hard errors.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CHAT_ALPN: &[u8] = b"ghostchat/1";
pub const PAIR_ALPN: &[u8] = b"ghostchat-pair/1";
/// v2 added voice calls; a v1 peer would not understand the call frames.
pub const PROTOCOL_VERSION: u16 = 2;

/// Maximum chat message body (PRD §9).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Maximum encoded frame. Leaves room for the largest valid `Text` frame.
pub const MAX_FRAME_BYTES: usize = 80 * 1024;

/// QUIC application close codes.
pub mod close {
    pub const NORMAL: u32 = 0;
    pub const PROTOCOL_ERROR: u32 = 1;
    pub const PROTOCOL_VERSION: u32 = 2;
    pub const DUPLICATE: u32 = 3;
    pub const PANIC: u32 = 4;
    pub const NOT_ALLOWED: u32 = 403;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    Hello {
        version: u16,
        codename: String,
    },
    Text {
        id: u64,
        body: String,
    },
    Ack {
        id: u64,
    },
    Codename {
        codename: String,
    },
    FileOffer {
        id: u64,
        name: String,
        size: u64,
        hash: [u8; 32],
    },
    FileAccept {
        id: u64,
    },
    FileReject {
        id: u64,
    },
    FileResult {
        id: u64,
        ok: bool,
    },
    Bye,
    /// Start a voice call. `id` tags the call's audio datagrams.
    CallOffer {
        id: u32,
    },
    CallAccept {
        id: u32,
    },
    /// Decline, cancel or hang up.
    CallEnd {
        id: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairFrame {
    Request { version: u16, codename: String },
    Response { accepted: bool, codename: String },
}

#[derive(Debug)]
pub enum ProtoError {
    Io(std::io::Error),
    TooLarge(usize),
    Decode(postcard::Error),
    /// The stream ended cleanly before a frame header.
    Closed,
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Io(e) => write!(f, "i/o error: {e}"),
            ProtoError::TooLarge(n) => write!(f, "frame too large ({n} bytes)"),
            ProtoError::Decode(e) => write!(f, "malformed frame: {e}"),
            ProtoError::Closed => write!(f, "stream closed"),
        }
    }
}

impl std::error::Error for ProtoError {}

impl From<std::io::Error> for ProtoError {
    fn from(e: std::io::Error) -> Self {
        ProtoError::Io(e)
    }
}

/// Decodes one frame payload (without the length prefix). Exposed for fuzzing.
pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtoError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtoError::TooLarge(payload.len()));
    }
    postcard::from_bytes(payload).map_err(ProtoError::Decode)
}

pub fn encode<T: Serialize>(frame: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = postcard::to_stdvec(frame).map_err(ProtoError::Decode)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtoError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub async fn write_frame<W, T>(w: &mut W, frame: &T) -> Result<(), ProtoError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = encode(frame)?;
    let res = w.write_all(&bytes).await;
    zeroize::Zeroize::zeroize(&mut bytes);
    res?;
    Ok(())
}

pub async fn read_frame<R, T>(r: &mut R) -> Result<T, ProtoError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(ProtoError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(ProtoError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let frame = decode(&buf);
    zeroize::Zeroize::zeroize(&mut buf);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_all_frames() {
        let frames = vec![
            Frame::Hello {
                version: PROTOCOL_VERSION,
                codename: "NIGHTOWL".into(),
            },
            Frame::Text {
                id: 7,
                body: "hello 你好".into(),
            },
            Frame::Ack { id: 7 },
            Frame::Codename {
                codename: "GHOST-01".into(),
            },
            Frame::FileOffer {
                id: 1,
                name: "a.txt".into(),
                size: 3,
                hash: [9; 32],
            },
            Frame::FileAccept { id: 1 },
            Frame::FileReject { id: 1 },
            Frame::FileResult { id: 1, ok: true },
            Frame::Bye,
        ];
        let (mut a, mut b) = tokio::io::duplex(1 << 20);
        for f in &frames {
            write_frame(&mut a, f).await.unwrap();
        }
        for f in &frames {
            let got: Frame = read_frame(&mut b).await.unwrap();
            assert_eq!(&got, f);
        }
    }

    #[tokio::test]
    async fn max_text_fits_in_frame() {
        let f = Frame::Text {
            id: u64::MAX,
            body: "x".repeat(MAX_TEXT_BYTES),
        };
        assert!(encode(&f).is_ok());
    }

    #[tokio::test]
    async fn rejects_oversized_length_before_allocating() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(u32::MAX).to_be_bytes()).await.unwrap();
        let r: Result<Frame, _> = read_frame(&mut b).await;
        assert!(matches!(r, Err(ProtoError::TooLarge(_))));
    }

    #[tokio::test]
    async fn rejects_unknown_variant() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // Variant index 200 does not exist.
        a.write_all(&[0, 0, 0, 2, 0xc8, 0x01]).await.unwrap();
        let r: Result<Frame, _> = read_frame(&mut b).await;
        assert!(matches!(r, Err(ProtoError::Decode(_))));
    }

    #[tokio::test]
    async fn clean_eof_is_closed() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        let r: Result<Frame, _> = read_frame(&mut b).await;
        assert!(matches!(r, Err(ProtoError::Closed)));
    }

    #[test]
    fn truncated_payload_is_error() {
        let bytes = encode(&Frame::Text {
            id: 1,
            body: "hello".into(),
        })
        .unwrap();
        assert!(decode::<Frame>(&bytes[4..bytes.len() - 2]).is_err());
    }
}
