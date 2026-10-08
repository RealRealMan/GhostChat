//! File transfer between two engines: accept, reject, limits, unsafe names,
//! corrupted and truncated data.

mod common;

use std::{path::PathBuf, time::Duration};

use common::Node;
use ghostchat::engine::{Command, Event, Presence};
use iroh::address_lookup::MemoryLookup;

async fn connected(max_b: u64) -> (Node, Node) {
    let lookup = MemoryLookup::new();
    let mut a = Node::start("ALICE", &lookup).await;
    let mut b = Node::start_with_limit("BOB", &lookup, max_b).await;
    a.add_contact(&b, "BOB");
    b.add_contact(&a, "ALICE");
    a.handle.send(Command::Connect(b.key));
    let ak = a.key;
    b.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == ak)
            .then_some(())
    })
    .await;
    let bk = b.key;
    a.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == bk)
            .then_some(())
    })
    .await;
    (a, b)
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

/// A sends `path` to B; returns the offer id seen by B.
async fn offer(a: &mut Node, b: &mut Node, path: PathBuf) -> (u64, String, bool) {
    a.handle.send(Command::SendFile { to: b.key, path });
    b.wait_for(|e| match e {
        Event::FileIncoming {
            id,
            name,
            executable,
            ..
        } => Some((*id, name.clone(), *executable)),
        _ => None,
    })
    .await
}

async fn done(n: &mut Node, incoming: bool) -> Result<Option<PathBuf>, String> {
    n.wait_for(|e| match e {
        Event::FileDone {
            incoming: i,
            result,
            ..
        } if *i == incoming => Some(result.clone()),
        _ => None,
    })
    .await
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_transfers_and_verifies() {
    let (mut a, mut b) = connected(1 << 30).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("photo.jpg");
    let content = data(3 * 1024 * 1024 + 17);
    std::fs::write(&path, &content).unwrap();

    let (id, name, exe) = offer(&mut a, &mut b, path).await;
    assert_eq!(name, "photo.jpg");
    assert!(!exe);
    // Nothing is written before the receiver accepts.
    assert!(files_in(b.downloads.path()).is_empty());

    let ak = a.key;
    b.handle.send(Command::FileDecision {
        peer: ak,
        id,
        accept: true,
    });
    let saved = done(&mut b, true).await.unwrap().unwrap();
    assert_eq!(std::fs::read(&saved).unwrap(), content);
    assert_eq!(files_in(b.downloads.path()), vec!["photo.jpg"]);
    assert_eq!(
        done(&mut a, false).await,
        Ok(None),
        "sender gets confirmation"
    );

    // Same name again gets a new, non-clobbering name.
    let path2 = src.path().join("photo.jpg");
    let (id2, ..) = offer(&mut a, &mut b, path2).await;
    b.handle.send(Command::FileDecision {
        peer: ak,
        id: id2,
        accept: true,
    });
    done(&mut b, true).await.unwrap();
    assert_eq!(
        files_in(b.downloads.path()),
        vec!["photo (2).jpg", "photo.jpg"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reject_sends_nothing() {
    let (mut a, mut b) = connected(1 << 30).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("notes.txt");
    std::fs::write(&path, b"secret").unwrap();
    let (id, ..) = offer(&mut a, &mut b, path).await;
    b.handle.send(Command::FileDecision {
        peer: a.key,
        id,
        accept: false,
    });
    let r = done(&mut a, false).await;
    assert!(matches!(r, Err(ref e) if e.contains("declined")), "{r:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(files_in(b.downloads.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn too_large_is_rejected_automatically() {
    let (mut a, mut b) = connected(1000).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("big.bin");
    std::fs::write(&path, data(5000)).unwrap();
    a.handle.send(Command::SendFile { to: b.key, path });
    let r = done(&mut a, false).await;
    assert!(r.is_err(), "{r:?}");
    let notice = b
        .wait_for(|e| match e {
            Event::Notice(n) => Some(n.clone()),
            Event::FileIncoming { .. } => panic!("must not be offered to the user"),
            _ => None,
        })
        .await;
    assert!(notice.contains("rejected automatically"), "{notice}");
}

#[tokio::test(flavor = "multi_thread")]
async fn executable_is_flagged() {
    let (mut a, mut b) = connected(1 << 30).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("setup.exe");
    std::fs::write(&path, b"MZ").unwrap();
    let (_, name, exe) = offer(&mut a, &mut b, path).await;
    assert_eq!(name, "setup.exe");
    assert!(exe);
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupted_data_is_deleted() {
    let (mut a, mut b) = connected(1 << 30).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("doc.pdf");
    std::fs::write(&path, data(200_000)).unwrap();
    let (id, ..) = offer(&mut a, &mut b, path.clone()).await;
    // Same length, different bytes after the hash was announced.
    std::fs::write(&path, vec![0u8; 200_000]).unwrap();
    b.handle.send(Command::FileDecision {
        peer: a.key,
        id,
        accept: true,
    });
    let r = done(&mut b, true).await;
    assert!(matches!(r, Err(ref e) if e.contains("integrity")), "{r:?}");
    assert!(
        files_in(b.downloads.path()).is_empty(),
        "partial file removed"
    );
    let r = done(&mut a, false).await;
    assert!(r.is_err(), "sender learns it failed: {r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn truncated_data_is_deleted() {
    let (mut a, mut b) = connected(1 << 30).await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("video.mp4");
    std::fs::write(&path, data(500_000)).unwrap();
    let (id, ..) = offer(&mut a, &mut b, path.clone()).await;
    std::fs::write(&path, data(100_000)).unwrap();
    b.handle.send(Command::FileDecision {
        peer: a.key,
        id,
        accept: true,
    });
    let r = done(&mut b, true).await;
    assert!(matches!(r, Err(ref e) if e.contains("incomplete")), "{r:?}");
    assert!(files_in(b.downloads.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_to_offline_contact_fails_cleanly() {
    let lookup = MemoryLookup::new();
    let mut a = Node::start("ALICE", &lookup).await;
    let b = Node::start("BOB", &lookup).await;
    a.add_contact(&b, "BOB");
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("x.txt");
    std::fs::write(&path, b"x").unwrap();
    a.handle.send(Command::SendFile { to: b.key, path });
    let w = a
        .wait_for(|e| match e {
            Event::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .await;
    assert!(w.contains("offline"), "{w}");
}
