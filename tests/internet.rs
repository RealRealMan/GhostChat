//! Real-network test through n0's public relay + discovery.
//! Ignored by default because it needs internet access:
//! `cargo test --test internet -- --ignored --nocapture`

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use ghostchat::{
    contacts::ContactBook,
    engine::{self, Command, EngineConfig, Event, NetConfig, PathKind, Presence},
};
use iroh::SecretKey;
use tokio::sync::mpsc;

struct N {
    key: iroh::PublicKey,
    handle: engine::EngineHandle,
    rx: mpsc::UnboundedReceiver<Event>,
    contacts: Arc<Mutex<ContactBook>>,
}

fn start(name: &str) -> N {
    start_with(name, false)
}

fn start_with(name: &str, relay_only: bool) -> N {
    let sk = SecretKey::generate();
    let key = sk.public();
    let contacts = Arc::new(Mutex::new(ContactBook::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = engine::spawn(
        &tokio::runtime::Handle::current(),
        EngineConfig {
            secret_key: sk,
            codename: name.into(),
            contacts: contacts.clone(),
            net: NetConfig::Internet {
                relay_urls: vec![],
                pkarr_relay: None,
                relay_only,
            },
            auto_connect: None,
            downloads: std::env::temp_dir(),
            max_file_size: 1 << 20,
        },
        tx,
        Arc::new(|_| {}),
    );
    N {
        key,
        handle,
        rx,
        contacts,
    }
}

async fn until<T>(
    rx: &mut mpsc::UnboundedReceiver<Event>,
    secs: u64,
    mut f: impl FnMut(&Event) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            let ev = rx.recv().await.expect("engine stopped");
            if let Some(v) = f(&ev) {
                return v;
            }
        }
    })
    .await
    .expect("timed out")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs internet (n0 relay + DNS discovery)"]
async fn chat_over_n0() {
    let mut a = start("ALICE");
    let mut b = start("BOB");
    a.contacts.lock().unwrap().add(b.key, "BOB".into());
    b.contacts.lock().unwrap().add(a.key, "ALICE".into());

    until(&mut a.rx, 30, |e| {
        matches!(e, Event::Network { online: true }).then_some(())
    })
    .await;
    until(&mut b.rx, 30, |e| {
        matches!(e, Event::Network { online: true }).then_some(())
    })
    .await;
    // Give pkarr publishing a moment.
    tokio::time::sleep(Duration::from_secs(3)).await;

    a.handle.send(Command::Connect(b.key));
    let bk = b.key;
    until(&mut a.rx, 60, |e| {
        if let Event::Notice(n) = e {
            println!("notice: {n}");
        }
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == bk)
            .then_some(())
    })
    .await;
    println!("connected via n0 discovery");

    a.handle.send(Command::SendText {
        to: b.key,
        id: 1,
        body: "over the internet".into(),
    });
    let ak = a.key;
    let body = until(&mut b.rx, 30, |e| match e {
        Event::Text { peer, body } if *peer == ak => Some(body.clone()),
        _ => None,
    })
    .await;
    assert_eq!(body, "over the internet");

    // Observe what path the status bar would show.
    let path = until(&mut a.rx, 10, |e| match e {
        Event::Path { path, .. } if *path != PathKind::Unknown => Some(*path),
        _ => None,
    })
    .await;
    println!("path reported: {path:?}");
    a.handle.send(Command::Shutdown { panic: false });
    b.handle.send(Command::Shutdown { panic: false });
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs internet (n0 relay + DNS discovery)"]
async fn chat_over_n0_relay_only() {
    let mut a = start_with("ALICE", true);
    let mut b = start_with("BOB", true);
    a.contacts.lock().unwrap().add(b.key, "BOB".into());
    b.contacts.lock().unwrap().add(a.key, "ALICE".into());
    for n in [&mut a, &mut b] {
        until(&mut n.rx, 30, |e| {
            matches!(e, Event::Network { online: true }).then_some(())
        })
        .await;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    a.handle.send(Command::Connect(b.key));
    let bk = b.key;
    until(&mut a.rx, 60, |e| {
        matches!(e, Event::Presence { peer, presence: Presence::Online } if *peer == bk)
            .then_some(())
    })
    .await;
    a.handle.send(Command::SendText {
        to: b.key,
        id: 1,
        body: "via relay".into(),
    });
    let ak = a.key;
    let body = until(&mut b.rx, 30, |e| match e {
        Event::Text { peer, body } if *peer == ak => Some(body.clone()),
        _ => None,
    })
    .await;
    assert_eq!(body, "via relay");
    let path = until(&mut a.rx, 10, |e| match e {
        Event::Path { path, .. } if *path != PathKind::Unknown => Some(*path),
        _ => None,
    })
    .await;
    println!("path reported: {path:?}");
    assert_eq!(path, PathKind::Relay, "status bar must say RELAY");
    a.handle.send(Command::Shutdown { panic: false });
    b.handle.send(Command::Shutdown { panic: false });
}
