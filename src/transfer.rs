//! File transfer helpers (PRD §10): hashing, streaming, safe destinations.
//!
//! Data stream layout (one QUIC uni stream per file): `u64 BE id || bytes`.
//! The receiver hashes while writing to `<name>.part` and only renames to the
//! final name if both size and BLAKE3 hash match the offer. Anything else
//! deletes the partial file.

use std::path::{Path, PathBuf};

use iroh::endpoint::{Connection, RecvStream};
use tokio::io::AsyncWriteExt;

pub const CHUNK: usize = 64 * 1024;
/// Progress events at most this often (bytes).
const PROGRESS_STEP: u64 = 1024 * 1024;

pub struct Offer {
    pub name: String,
    pub size: u64,
    pub hash: [u8; 32],
}

/// Size and BLAKE3 hash of a local file, refusing files above `max`.
pub fn hash_file(path: &Path, max: u64) -> Result<(u64, [u8; 32]), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read file: {e}"))?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > max {
        return Err(format!(
            "file too large ({} — limit is {})",
            human_size(meta.len()),
            human_size(max)
        ));
    }
    let mut hasher = blake3::Hasher::new();
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
    hasher
        .update_reader(file)
        .map_err(|e| format!("cannot read file: {e}"))?;
    Ok((meta.len(), *hasher.finalize().as_bytes()))
}

/// `dir/name`, or `dir/name (2).ext` etc. if that already exists.
/// `name` must already be sanitized.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() && !part_path(&first).exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists() && !part_path(p).exists())
        .expect("unbounded search")
}

pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Streams a file on a new uni stream. `progress(done)` is called periodically.
pub async fn send(
    conn: &Connection,
    id: u64,
    path: &Path,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("cannot open file: {e}"))?;
    let mut stream = conn.open_uni().await.map_err(|e| e.to_string())?;
    stream
        .write_all(&id.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut last = 0u64;
    loop {
        let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf)
            .await
            .map_err(|e| format!("cannot read file: {e}"))?;
        if n == 0 {
            break;
        }
        stream
            .write_all(&buf[..n])
            .await
            .map_err(|e| e.to_string())?;
        done += n as u64;
        if done - last >= PROGRESS_STEP {
            last = done;
            progress(done);
        }
    }
    stream.finish().map_err(|e| e.to_string())?;
    progress(done);
    Ok(())
}

/// Receives into `final_path` (via `.part`). On any failure the partial file
/// is deleted and a user-readable reason returned.
pub async fn receive(
    recv: &mut RecvStream,
    final_path: &Path,
    offer: &Offer,
    progress: impl FnMut(u64),
) -> Result<(), String> {
    let part = part_path(final_path);
    let res = receive_inner(recv, &part, offer, progress).await;
    match res {
        Ok(()) => tokio::fs::rename(&part, final_path).await.map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("could not save file: {e}")
        }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

async fn receive_inner(
    recv: &mut RecvStream,
    part: &Path,
    offer: &Offer,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| format!("could not create file: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut last = 0u64;
    loop {
        let n = match recv.read(&mut buf).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(e) => return Err(format!("transfer interrupted ({e})")),
        };
        done += n as u64;
        if done > offer.size {
            return Err("peer sent more data than announced".into());
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .await
            .map_err(|e| format!("could not write file: {e}"))?;
        if done - last >= PROGRESS_STEP {
            last = done;
            progress(done);
        }
    }
    file.flush()
        .await
        .map_err(|e| format!("could not write file: {e}"))?;
    drop(file);
    if done != offer.size {
        return Err(format!(
            "transfer incomplete ({} of {})",
            human_size(done),
            human_size(offer.size)
        ));
    }
    if hasher.finalize().as_bytes() != &offer.hash {
        return Err("integrity check failed: the file was corrupted or changed".into());
    }
    progress(done);
    Ok(())
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(unique_path(d, "a.txt"), d.join("a.txt"));
        std::fs::write(d.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(d, "a.txt"), d.join("a (2).txt"));
        std::fs::write(d.join("a (2).txt"), b"x").unwrap();
        assert_eq!(unique_path(d, "a.txt"), d.join("a (3).txt"));
        std::fs::write(d.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(d, "noext"), d.join("noext (2)"));
        // An in-progress download also reserves its name.
        std::fs::write(d.join("b.bin.part"), b"x").unwrap();
        assert_eq!(unique_path(d, "b.bin"), d.join("b (2).bin"));
    }

    #[test]
    fn hash_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello").unwrap();
        let (size, hash) = hash_file(&p, 10).unwrap();
        assert_eq!(size, 5);
        assert_eq!(&hash, blake3::hash(b"hello").as_bytes());
        assert!(hash_file(&p, 4).unwrap_err().contains("too large"));
        assert!(hash_file(dir.path(), 10).is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
