//! A hand-written hostile peer that speaks the wire protocol directly, to check
//! that GhostChat treats all peer input as untrusted (PRD §16).

mod common;

use std::time::Duration;

use common::Node;
use ghostchat::{
    engine::{Event, Presence},
    proto::{self, CHAT_ALPN, Frame, PROTOCOL_VERSION},
};
use iroh::{
    Endpoint, RelayMode, SecretKey,
    address_lookup::MemoryLookup,
    endpoint::{Connection, RecvStream, SendStream, presets},
};

struct Evil {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    _ep: Endpoint,
}

/// Connects to `victim` as a contact the victim knows (so the allowlist lets
/// it in) and completes the Hello exchange.
async fn evil_session(
    victim: &mut Node,
    lookup: &MemoryLookup,
    hello_version: u16,
) -> Result<Evil, String> {
    let sk = SecretKey::generate();
    victim
        .contacts
        .lock()
        .unwrap()
        .add(sk.public(), "MALLORY".into());
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .secret_key(sk)
        .bind()
        .await
        .unwrap();
    let conn = ep
        .connect(victim.key, CHAT_ALPN)
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    proto::write_frame(
        &mut send,
        &Frame::Hello {
            version: hello_version,
            codename: "MALLORY\u{202e}\x1b[31m".into(),
        },
    )
    .await
    .unwrap();
    let reply: Frame = proto::read_frame(&mut recv)
        .await
        .map_err(|e| e.to_string())?;
    assert!(matches!(reply, Frame::Hello { .. }));
    Ok(Evil {
        conn,
        send,
        recv,
        _ep: ep,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_file_offer_name_is_rejected() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let mut evil = evil_session(&mut victim, &lookup, PROTOCOL_VERSION)
        .await
        .unwrap();

    for (id, name) in [(1u64, "../../AppData/evil.exe"), (2, "CON"), (3, "..")] {
        proto::write_frame(
            &mut evil.send,
            &Frame::FileOffer {
                id,
                name: name.into(),
                size: 10,
                hash: [0; 32],
            },
        )
        .await
        .unwrap();
        let reply: Frame = proto::read_frame(&mut evil.recv).await.unwrap();
        assert_eq!(reply, Frame::FileReject { id }, "{name}");
    }
    let warnings: Vec<String> = victim
        .drain_for(Duration::from_millis(300))
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::Warning(w) => Some(w),
            Event::FileIncoming { name, .. } => panic!("unsafe offer reached the user: {name}"),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .all(|w| w.contains("rejected automatically"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn path_components_are_stripped_from_offers() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let mut evil = evil_session(&mut victim, &lookup, PROTOCOL_VERSION)
        .await
        .unwrap();
    proto::write_frame(
        &mut evil.send,
        &Frame::FileOffer {
            id: 7,
            name: "C:\\Windows\\System32\\photo\u{202e}gpj.exe".into(),
            size: 10,
            hash: [0; 32],
        },
    )
    .await
    .unwrap();
    let (name, exe) = victim
        .wait_for(|e| match e {
            Event::FileIncoming {
                name, executable, ..
            } => Some((name.clone(), *executable)),
            _ => None,
        })
        .await;
    assert_eq!(name, "photogpj.exe");
    assert!(exe, "bidi trick must not hide the .exe");
}

#[tokio::test(flavor = "multi_thread")]
async fn unsolicited_data_stream_is_not_written() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let evil = evil_session(&mut victim, &lookup, PROTOCOL_VERSION)
        .await
        .unwrap();
    // Push file data that nobody accepted.
    let mut s = evil.conn.open_uni().await.unwrap();
    s.write_all(&42u64.to_be_bytes()).await.unwrap();
    let _ = s.write_all(&[0xAA; 100_000]).await;
    let _ = s.finish();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let files: Vec<_> = std::fs::read_dir(victim.downloads.path())
        .unwrap()
        .collect();
    assert!(files.is_empty(), "nothing may be written without consent");
    // The chat channel itself keeps working.
    let events = victim.drain_for(Duration::from_millis(200)).await;
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::Presence {
            presence: Presence::Offline,
            ..
        }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_codename_and_text_are_sanitized() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let mut evil = evil_session(&mut victim, &lookup, PROTOCOL_VERSION)
        .await
        .unwrap();
    proto::write_frame(
        &mut evil.send,
        &Frame::Text {
            id: 1,
            body: "hi\x1b]0;pwned\x07\u{202e}there\x1b[2J".into(),
        },
    )
    .await
    .unwrap();
    let body = victim
        .wait_for(|e| match e {
            Event::Text { body, .. } => Some(body.clone()),
            _ => None,
        })
        .await;
    assert_eq!(body, "hithere");
    // The invalid codename in Hello was ignored, not stored.
    let c = victim
        .contacts
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.codename.starts_with("MALLORY"))
        .map(|c| c.codename.clone());
    assert_eq!(c.as_deref(), Some("MALLORY"));
}

#[tokio::test(flavor = "multi_thread")]
async fn garbage_frame_closes_session() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let mut evil = evil_session(&mut victim, &lookup, PROTOCOL_VERSION)
        .await
        .unwrap();
    victim
        .wait_for(|e| {
            matches!(
                e,
                Event::Presence {
                    presence: Presence::Online,
                    ..
                }
            )
            .then_some(())
        })
        .await;
    tokio::io::AsyncWriteExt::write_all(&mut evil.send, &[0, 0, 0, 3, 0xff, 0xff, 0xff])
        .await
        .unwrap();
    let notice = victim
        .wait_for(|e| match e {
            Event::Notice(n) if n.contains("secure channel closed") => Some(n.clone()),
            _ => None,
        })
        .await;
    assert!(notice.contains("protocol error"), "{notice}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_protocol_version_is_refused() {
    let lookup = MemoryLookup::new();
    let mut victim = Node::start("VICTIM", &lookup).await;
    let r = evil_session(&mut victim, &lookup, 99).await;
    assert!(r.is_err(), "victim must not answer a v99 Hello");
    let events = victim.drain_for(Duration::from_millis(300)).await;
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::Presence {
            presence: Presence::Online,
            ..
        }
    )));
}
