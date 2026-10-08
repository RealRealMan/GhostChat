//! Two engines on one machine: connect, chat, disconnect, allowlist.

mod common;

use std::time::Duration;

use common::Node;
use ghostchat::engine::{Command, Delivery, Event, Presence};
use iroh::address_lookup::MemoryLookup;

async fn pair_up() -> (Node, Node) {
    let lookup = MemoryLookup::new();
    let a = Node::start("ALICE", &lookup).await;
    let b = Node::start("BOB", &lookup).await;
    a.add_contact(&b, "BOB");
    b.add_contact(&a, "ALICE");
    (a, b)
}

async fn connect(a: &mut Node, b: &mut Node) {
    a.handle.send(Command::Connect(b.key));
    let bk = b.key;
    a.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == bk)
            .then_some(())
    })
    .await;
    let ak = a.key;
    b.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == ak)
            .then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_chat_disconnect() {
    let (mut a, mut b) = pair_up().await;
    connect(&mut a, &mut b).await;

    a.handle.send(Command::SendText {
        to: b.key,
        id: 1,
        body: "hello \x1b[31mbob\u{202e}".into(),
    });
    let ak = a.key;
    let body = b
        .wait_for(|e| match e {
            Event::Text { peer, body } if *peer == ak => Some(body.clone()),
            _ => None,
        })
        .await;
    assert_eq!(body, "hello bob", "peer text must arrive sanitized");

    let bk = b.key;
    a.wait_for(|e| {
        matches!(e, Event::Delivery { peer, id: 1, status: Delivery::Delivered } if *peer == bk)
            .then_some(())
    })
    .await;

    // Reply the other way.
    b.handle.send(Command::SendText {
        to: a.key,
        id: 9,
        body: "hi alice".into(),
    });
    let body = a
        .wait_for(|e| match e {
            Event::Text { peer, body } if *peer == bk => Some(body.clone()),
            _ => None,
        })
        .await;
    assert_eq!(body, "hi alice");

    a.handle.send(Command::Disconnect(b.key));
    b.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Offline } if *peer == ak)
            .then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_key_is_rejected() {
    let lookup = MemoryLookup::new();
    let mut a = Node::start("ALICE", &lookup).await;
    let mut stranger = Node::start("STRANGER", &lookup).await;
    // The stranger knows Alice, but Alice does not know the stranger.
    stranger.add_contact(&a, "ALICE");
    stranger.handle.send(Command::Connect(a.key));
    let ak = a.key;
    let notice = stranger
        .wait_for(|e| match e {
            Event::Notice(n) => Some(n.clone()),
            Event::Presence {
                peer,
                presence: Presence::Online,
            } if *peer == ak => panic!("stranger must not get a session"),
            _ => None,
        })
        .await;
    assert!(notice.contains("not reachable"), "{notice}");

    // Alice never saw a session or a message.
    let events = a.drain_for(Duration::from_millis(500)).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            Event::Presence { .. } | Event::Text { .. } | Event::Handshake { .. }
        )),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_contact_send_fails_without_queueing() {
    let (mut a, b) = pair_up().await;
    let bk = b.key;
    a.handle.send(Command::SendText {
        to: bk,
        id: 3,
        body: "anyone?".into(),
    });
    let status = a
        .wait_for(|e| match e {
            Event::Delivery { id: 3, status, .. } => Some(status.clone()),
            _ => None,
        })
        .await;
    assert!(
        matches!(status, Delivery::Failed(ref m) if m.contains("offline")),
        "{status:?}"
    );

    // Stop B entirely; dialing must fail cleanly.
    b.handle.send(Command::Shutdown { panic: false });
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.handle.send(Command::Connect(bk));
    a.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Offline } if *peer == bk)
            .then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_message_is_refused_locally() {
    let (mut a, mut b) = pair_up().await;
    connect(&mut a, &mut b).await;
    a.handle.send(Command::SendText {
        to: b.key,
        id: 5,
        body: "x".repeat(ghostchat::proto::MAX_TEXT_BYTES + 1),
    });
    let status = a
        .wait_for(|e| match e {
            Event::Delivery { id: 5, status, .. } => Some(status.clone()),
            _ => None,
        })
        .await;
    assert!(matches!(status, Delivery::Failed(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_dials_end_with_one_session() {
    let (mut a, mut b) = pair_up().await;
    a.handle.send(Command::Connect(b.key));
    b.handle.send(Command::Connect(a.key));
    let bk = b.key;
    a.wait_for(|e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == bk)
            .then_some(())
    })
    .await;
    // Let any duplicate get resolved, then check messages still flow.
    tokio::time::sleep(Duration::from_secs(1)).await;
    a.handle.send(Command::SendText {
        to: b.key,
        id: 1,
        body: "still here".into(),
    });
    let ak = a.key;
    b.wait_for(|e| match e {
        Event::Text { peer, body } if *peer == ak && body == "still here" => Some(()),
        _ => None,
    })
    .await;
    a.wait_for(|e| {
        matches!(
            e,
            Event::Delivery {
                id: 1,
                status: Delivery::Delivered,
                ..
            }
        )
        .then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codename_change_is_announced() {
    let (mut a, mut b) = pair_up().await;
    connect(&mut a, &mut b).await;
    a.handle.send(Command::SetCodename("GHOST-01".into()));
    let ak = a.key;
    let (old, new) = b
        .wait_for(|e| match e {
            Event::CodenameChanged { peer, old, new } if *peer == ak => {
                Some((old.clone(), new.clone()))
            }
            _ => None,
        })
        .await;
    assert_eq!((old.as_str(), new.as_str()), ("ALICE", "GHOST-01"));
    assert_eq!(b.contacts.lock().unwrap().display_name(&ak), "GHOST-01");
}
