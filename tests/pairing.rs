//! Remote pairing (`/invite` + `/add`) and trust enforcement.

mod common;

use std::time::Duration;

use common::Node;
use ghostchat::{
    contact_key::ContactKey,
    contacts::{AddOutcome, Trust},
    engine::{Command, Event, Presence},
};
use iroh::address_lookup::MemoryLookup;

fn contact_key(n: &Node, codename: &str) -> ContactKey {
    ContactKey::new(n.key, codename.into())
}

async fn open_window(n: &mut Node) {
    n.handle
        .send(Command::OpenPairing(Duration::from_secs(600)));
    n.wait_for(|e| matches!(e, Event::PairingWindow { open: true }).then_some(()))
        .await;
}

/// Runs the full invite flow; returns the outcome seen by the inviter.
async fn pair(inviter: &mut Node, invitee: &mut Node, inviter_name: &str) -> AddOutcome {
    open_window(inviter).await;
    invitee
        .handle
        .send(Command::Pair(contact_key(inviter, inviter_name)));
    let invitee_key = invitee.key;
    let (peer, outcome) = inviter
        .wait_for(|e| match e {
            Event::PairRequest { peer, outcome, .. } => Some((*peer, outcome.clone())),
            _ => None,
        })
        .await;
    assert_eq!(peer, invitee_key, "requester identity comes from TLS");
    inviter
        .handle
        .send(Command::PairDecision { peer, accept: true });
    let ik = inviter.key;
    invitee
        .wait_for(|e| matches!(e, Event::Paired { peer, .. } if *peer == ik).then_some(()))
        .await;
    outcome
}

#[tokio::test(flavor = "multi_thread")]
async fn invite_add_accept_then_chat() {
    let lookup = MemoryLookup::new();
    let mut inviter = Node::start("INVITER", &lookup).await;
    let mut contact = Node::start("NIGHTOWL", &lookup).await;

    let outcome = pair(&mut inviter, &mut contact, "INVITER").await;
    assert_eq!(outcome, AddOutcome::New);

    for (n, other, name) in [
        (&inviter, &contact, "NIGHTOWL"),
        (&contact, &inviter, "INVITER"),
    ] {
        let book = n.contacts.lock().unwrap();
        let c = book.get(&other.key).expect("contact added on both sides");
        assert_eq!(c.codename, name);
        assert_eq!(c.trust, Trust::Added, "pairing never grants VERIFIED");
    }

    // The invitee dials the chat channel right after pairing.
    let mk = contact.key;
    inviter
        .wait_for(|e| {
            matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == mk)
                .then_some(())
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_request_outside_window_is_dropped_silently() {
    let lookup = MemoryLookup::new();
    let mut inviter = Node::start("INVITER", &lookup).await;
    let mut stranger = Node::start("STRANGER", &lookup).await;

    stranger
        .handle
        .send(Command::Pair(contact_key(&inviter, "INVITER")));
    let warning = stranger
        .wait_for(|e| match e {
            Event::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .await;
    assert!(warning.contains("pairing with INVITER failed"), "{warning}");

    let events = inviter.drain_for(Duration::from_millis(500)).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::PairRequest { .. })),
        "{events:?}"
    );
    assert!(inviter.contacts.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn window_closes_again() {
    let lookup = MemoryLookup::new();
    let mut inviter = Node::start("INVITER", &lookup).await;
    let mut late = Node::start("LATE", &lookup).await;
    inviter
        .handle
        .send(Command::OpenPairing(Duration::from_millis(300)));
    inviter
        .wait_for(|e| matches!(e, Event::PairingWindow { open: false }).then_some(()))
        .await;
    late.handle
        .send(Command::Pair(contact_key(&inviter, "INVITER")));
    late.wait_for(|e| matches!(e, Event::Warning(_)).then_some(()))
        .await;
    assert!(inviter.contacts.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_request_adds_nobody() {
    let lookup = MemoryLookup::new();
    let mut inviter = Node::start("INVITER", &lookup).await;
    let mut someone = Node::start("SOMEONE", &lookup).await;
    open_window(&mut inviter).await;
    someone
        .handle
        .send(Command::Pair(contact_key(&inviter, "INVITER")));
    let peer = inviter
        .wait_for(|e| match e {
            Event::PairRequest { peer, .. } => Some(*peer),
            _ => None,
        })
        .await;
    inviter.handle.send(Command::PairDecision {
        peer,
        accept: false,
    });
    let w = someone
        .wait_for(|e| match e {
            Event::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .await;
    assert!(w.contains("declined"), "{w}");
    assert!(inviter.contacts.lock().unwrap().is_empty());
    assert!(someone.contacts.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_key_is_blocked_until_reverified() {
    let lookup = MemoryLookup::new();
    let mut inviter = Node::start("INVITER", &lookup).await;
    let mut old_contact = Node::start("NIGHTOWL", &lookup).await;
    pair(&mut inviter, &mut old_contact, "INVITER").await;
    inviter
        .contacts
        .lock()
        .unwrap()
        .verify(&old_contact.key)
        .unwrap();
    old_contact.handle.send(Command::Shutdown { panic: false });

    // The contact "reinstalls": new key, same codename.
    let mut new_contact = Node::start("NIGHTOWL", &lookup).await;
    let outcome = pair(&mut inviter, &mut new_contact, "INVITER").await;
    assert_eq!(
        outcome,
        AddOutcome::KeyChanged {
            old_key: old_contact.key
        }
    );
    {
        let book = inviter.contacts.lock().unwrap();
        assert_eq!(book.get(&new_contact.key).unwrap().trust, Trust::KeyChanged);
        assert!(book.get(&old_contact.key).is_none());
    }

    // New key cannot open a chat channel while KEY_CHANGED.
    new_contact.handle.send(Command::Connect(inviter.key));
    let dk = inviter.key;
    new_contact
        .wait_for(|e| {
            matches!(e, Event::Presence { peer, presence: Presence::Offline } if *peer == dk)
                .then_some(())
        })
        .await;
    let events = inviter.drain_for(Duration::from_millis(500)).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            Event::Presence {
                presence: Presence::Online,
                ..
            }
        )),
        "{events:?}"
    );

    // After re-verification the connection is allowed.
    inviter
        .contacts
        .lock()
        .unwrap()
        .verify(&new_contact.key)
        .unwrap();
    // (Dialed from the inviter's side: new_contact may still have a rejected dial in flight.)
    inviter.handle.send(Command::Connect(new_contact.key));
    let nk = new_contact.key;
    inviter
        .wait_for(|e| {
            matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == nk)
                .then_some(())
        })
        .await;
}
