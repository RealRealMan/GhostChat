//! Helpers for running two engines in one process over loopback, with no
//! relay and no discovery server.

#![allow(dead_code)]

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ghostchat::{
    contacts::ContactBook,
    engine::{self, EngineConfig, EngineHandle, Event, NetConfig},
};
use iroh::{PublicKey, SecretKey, address_lookup::MemoryLookup};
use tokio::sync::mpsc;

pub const WAIT: Duration = Duration::from_secs(30);

pub struct Node {
    pub key: PublicKey,
    pub handle: EngineHandle,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub contacts: Arc<Mutex<ContactBook>>,
    pub downloads: tempfile::TempDir,
    /// How many events the engine flagged as needing the user's attention.
    pub attention: Arc<AtomicUsize>,
}

impl Node {
    pub async fn start(codename: &str, lookup: &MemoryLookup) -> Node {
        Self::start_with_key(SecretKey::generate(), codename, lookup).await
    }

    pub async fn start_with_key(sk: SecretKey, codename: &str, lookup: &MemoryLookup) -> Node {
        Self::start_full(sk, codename, lookup, 1 << 30).await
    }

    pub async fn start_with_limit(codename: &str, lookup: &MemoryLookup, max: u64) -> Node {
        Self::start_full(SecretKey::generate(), codename, lookup, max).await
    }

    async fn start_full(sk: SecretKey, codename: &str, lookup: &MemoryLookup, max: u64) -> Node {
        let downloads = tempfile::tempdir().unwrap();
        let key = sk.public();
        let contacts = Arc::new(Mutex::new(ContactBook::default()));
        let (tx, events) = mpsc::unbounded_channel();
        let attention = Arc::new(AtomicUsize::new(0));
        let counter = attention.clone();
        let handle = engine::spawn(
            &tokio::runtime::Handle::current(),
            EngineConfig {
                secret_key: sk,
                codename: codename.into(),
                contacts: contacts.clone(),
                net: NetConfig::Local {
                    lookup: lookup.clone(),
                },
                auto_connect: None,
                downloads: downloads.path().to_path_buf(),
                max_file_size: max,
            },
            tx,
            Arc::new(move |wants| {
                if wants {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            }),
        );
        let mut node = Node {
            key,
            handle,
            events,
            contacts,
            downloads,
            attention,
        };
        let addr = node
            .wait_for(|e| match e {
                Event::Ready { addr } => Some(addr.clone()),
                _ => None,
            })
            .await;
        lookup.add_endpoint_info(addr);
        node
    }

    pub fn attention_count(&self) -> usize {
        self.attention.load(Ordering::SeqCst)
    }

    pub fn add_contact(&self, other: &Node, codename: &str) {
        self.contacts
            .lock()
            .unwrap()
            .add(other.key, codename.into());
    }

    /// Waits for the first event `f` maps to `Some`, skipping others.
    pub async fn wait_for<T>(&mut self, mut f: impl FnMut(&Event) -> Option<T>) -> T {
        tokio::time::timeout(WAIT, async {
            loop {
                let ev = self.events.recv().await.expect("engine stopped");
                if let Some(v) = f(&ev) {
                    return v;
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }

    /// Collects events for `d` and returns them.
    pub async fn drain_for(&mut self, d: Duration) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = tokio::time::timeout(d, async {
            while let Some(ev) = self.events.recv().await {
                out.push(ev);
            }
        })
        .await;
        out
    }
}
