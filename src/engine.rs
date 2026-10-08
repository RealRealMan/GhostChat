//! Networking engine: owns the iroh endpoint, enforces the contact allowlist,
//! runs chat sessions. The UI talks to it only through [`Command`] / [`Event`].

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey,
    address_lookup::{MemoryLookup, PkarrPublisher, PkarrResolver},
    endpoint::{
        AfterHandshakeOutcome, Connection, EndpointHooks, RecvStream, SendStream, Side, presets,
    },
};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};
use zeroize::Zeroize;

use crate::{
    codename,
    contact_key::ContactKey,
    contacts::{AddOutcome, ContactBook},
    proto::{self, CHAT_ALPN, Frame, PAIR_ALPN, PROTOCOL_VERSION, PairFrame, close},
    sanitize,
    transfer::{self, Offer},
};

const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PATH_POLL: Duration = Duration::from_secs(1);
/// Unanswered pair requests kept at once. While the pairing window is open
/// anyone with the Contact Key can ask; extra requests are dropped silently so
/// a flood of fresh keys cannot grow memory or bury the real request.
const MAX_PENDING_PAIRS: usize = 8;
/// Unanswered + in-progress incoming files per contact.
const MAX_PENDING_FILES: usize = 16;
/// How long an invitee waits for the inviter to press accept.
const PAIR_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Where to find peers. `Internet` uses n0 (or self-hosted) relay + discovery.
#[derive(Debug, Clone)]
pub enum NetConfig {
    Internet {
        /// Empty = n0 default relays.
        relay_urls: Vec<RelayUrl>,
        /// `None` = n0 DNS/pkarr discovery.
        pkarr_relay: Option<url::Url>,
        /// Never use direct IP paths: all traffic goes through the relay, so
        /// contacts never learn this computer's IP address.
        relay_only: bool,
    },
    /// No relay, no discovery: peers are found via an in-memory address book.
    /// Used by integration tests on one machine.
    Local { lookup: MemoryLookup },
}

pub struct EngineConfig {
    pub secret_key: SecretKey,
    pub codename: String,
    pub contacts: Arc<Mutex<ContactBook>>,
    pub net: NetConfig,
    /// Dial every allowed contact without a session at this interval.
    pub auto_connect: Option<Duration>,
    /// Folder for received files.
    pub downloads: std::path::PathBuf,
    pub max_file_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Offline,
    Connecting,
    Online,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Unknown,
    Direct,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Delivered,
    Failed(String),
}

#[derive(Debug)]
pub enum Command {
    Connect(PublicKey),
    Disconnect(PublicKey),
    SendText {
        to: PublicKey,
        id: u64,
        body: String,
    },
    SetCodename(String),
    /// Inviter: accept pair requests for this long (`/invite`).
    OpenPairing(Duration),
    ClosePairing,
    /// Invitee: send a pair request to the owner of this Contact Key (`/add`).
    Pair(ContactKey),
    /// Inviter: answer an incoming pair request.
    PairDecision {
        peer: PublicKey,
        accept: bool,
    },
    /// Offer a file to a connected contact (`/send`).
    SendFile {
        to: PublicKey,
        path: PathBuf,
    },
    /// Answer an incoming file offer.
    FileDecision {
        peer: PublicKey,
        id: u64,
        accept: bool,
    },
    /// The UI changed the contact book (verify/remove): drop sessions that are
    /// no longer allowed.
    ContactsUpdated,
    /// Close everything. `panic` uses the PANIC close code.
    Shutdown {
        panic: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ready {
        addr: EndpointAddr,
    },
    BindFailed(String),
    /// Home relay reachability (true once the endpoint is online).
    Network {
        online: bool,
    },
    Presence {
        peer: PublicKey,
        presence: Presence,
    },
    Path {
        peer: PublicKey,
        path: PathKind,
    },
    /// A secure channel was just established (drives the handshake animation).
    Handshake {
        peer: PublicKey,
    },
    Text {
        peer: PublicKey,
        body: String,
    },
    Delivery {
        peer: PublicKey,
        id: u64,
        status: Delivery,
    },
    CodenameChanged {
        peer: PublicKey,
        old: String,
        new: String,
    },
    ContactsChanged,
    PairingWindow {
        open: bool,
    },
    /// Inviter side: someone pasted our Contact Key and asks to pair.
    /// `outcome` previews what accepting would do (e.g. a key change).
    PairRequest {
        peer: PublicKey,
        codename: String,
        outcome: AddOutcome,
    },
    /// Pairing finished on this side; the contact was added.
    Paired {
        peer: PublicKey,
        outcome: AddOutcome,
    },
    /// We offered a file; waiting for the contact to accept.
    FileOffered {
        peer: PublicKey,
        id: u64,
        name: String,
        size: u64,
    },
    /// A contact offers us a file (nothing is transferred before we accept).
    FileIncoming {
        peer: PublicKey,
        id: u64,
        name: String,
        size: u64,
        executable: bool,
    },
    FileProgress {
        peer: PublicKey,
        id: u64,
        incoming: bool,
        done: u64,
        total: u64,
    },
    /// Transfer finished. Incoming success carries the saved path.
    FileDone {
        peer: PublicKey,
        id: u64,
        incoming: bool,
        result: Result<Option<PathBuf>, String>,
    },
    Notice(String),
    Warning(String),
    Stopped,
}

impl Event {
    /// Something the user should notice even when GhostChat is in the
    /// background: a message, a pair request or an incoming file offer.
    pub fn wants_attention(&self) -> bool {
        matches!(
            self,
            Event::Text { .. } | Event::PairRequest { .. } | Event::FileIncoming { .. }
        )
    }
}

pub type EventSender = mpsc::UnboundedSender<Event>;
/// Called after each event; the flag is [`Event::wants_attention`].
pub type Notify = Arc<dyn Fn(bool) + Send + Sync>;

/// State the endpoint hook needs synchronously.
#[derive(Debug)]
pub struct Shared {
    pub contacts: Arc<Mutex<ContactBook>>,
    pub pairing_until: Mutex<Option<Instant>>,
}

impl Shared {
    fn allowed_chat(&self, key: &PublicKey) -> bool {
        self.contacts
            .lock()
            .expect("contacts lock")
            .can_connect(key)
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing_until
            .lock()
            .expect("pairing lock")
            .is_some_and(|t| Instant::now() < t)
    }
}

/// Rejects every incoming connection that is not allowed, right after the TLS
/// handshake and before any stream is accepted (PRD §6).
#[derive(Debug)]
struct Gate(Arc<Shared>);

impl EndpointHooks for Gate {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if conn.side() == Side::Client {
            // Outgoing: we only ever dial keys we chose.
            return AfterHandshakeOutcome::Accept;
        }
        let allowed = match conn.alpn() {
            a if a == CHAT_ALPN => self.0.allowed_chat(&conn.remote_id()),
            a if a == PAIR_ALPN => self.0.pairing_open(),
            _ => false,
        };
        if allowed {
            AfterHandshakeOutcome::Accept
        } else {
            AfterHandshakeOutcome::Reject {
                error_code: close::NOT_ALLOWED.into(),
                reason: Vec::new(),
            }
        }
    }
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Command>,
    pub shared: Arc<Shared>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Command) {
        // The engine only stops after Shutdown; a closed channel just means we
        // are already shutting down.
        let _ = self.tx.send(cmd);
    }
}

/// Starts the engine on the given runtime. Events are delivered on `events`;
/// `notify` is called after every event so the UI can repaint; its argument
/// is [`Event::wants_attention`], so the UI can alert the user even while its
/// own loop is paused (minimized window).
pub fn spawn(
    rt: &tokio::runtime::Handle,
    cfg: EngineConfig,
    events: EventSender,
    notify: Notify,
) -> EngineHandle {
    let shared = Arc::new(Shared {
        contacts: cfg.contacts.clone(),
        pairing_until: Mutex::new(None),
    });
    let (tx, rx) = mpsc::unbounded_channel();
    let sink = Sink { events, notify };
    let shared2 = shared.clone();
    rt.spawn(async move {
        match bind(&cfg, shared2.clone()).await {
            Ok(ep) => {
                if let NetConfig::Local { lookup } = &cfg.net {
                    lookup.add_endpoint_info(ep.addr());
                }
                Engine::new(ep, cfg, shared2, sink).run(rx).await
            }
            Err(e) => sink.emit(Event::BindFailed(e)),
        }
    });
    EngineHandle { tx, shared }
}

async fn bind(cfg: &EngineConfig, shared: Arc<Shared>) -> Result<Endpoint, String> {
    let alpns = vec![CHAT_ALPN.to_vec(), PAIR_ALPN.to_vec()];
    let builder = match &cfg.net {
        NetConfig::Internet {
            relay_urls,
            pkarr_relay,
            relay_only,
        } => {
            let mut b = match pkarr_relay {
                None => Endpoint::builder(presets::N0),
                Some(url) => Endpoint::builder(presets::Minimal)
                    .address_lookup(PkarrPublisher::builder(url.clone()))
                    .address_lookup(PkarrResolver::builder(url.clone()))
                    .relay_mode(RelayMode::Default),
            };
            if !relay_urls.is_empty() {
                b = b.relay_mode(RelayMode::custom(relay_urls.iter().cloned()));
            }
            if *relay_only {
                b = b.clear_ip_transports();
            }
            b
        }
        NetConfig::Local { lookup } => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .address_lookup(lookup.clone()),
    };
    builder
        .secret_key(cfg.secret_key.clone())
        .alpns(alpns)
        .hooks(Gate(shared))
        .bind()
        .await
        .map_err(|e| format!("could not start network: {e}"))
}

struct Sink {
    events: EventSender,
    notify: Notify,
}

impl Sink {
    fn emit(&self, ev: Event) {
        let attention = ev.wants_attention();
        if self.events.send(ev).is_ok() {
            (self.notify)(attention);
        }
    }
}

enum Internal {
    Established {
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        codename: Option<String>,
    },
    DialFailed {
        peer: PublicKey,
        reason: String,
    },
    Frame {
        peer: PublicKey,
        conn_id: usize,
        frame: Frame,
    },
    Ended {
        peer: PublicKey,
        conn_id: usize,
        reason: String,
    },
    PairIncoming {
        conn: Connection,
        send: SendStream,
        codename: String,
    },
    /// Invitee: the inviter answered (`Ok(codename)` = accepted).
    PairAnswered {
        key: ContactKey,
        result: Result<Option<String>, String>,
    },
    FileHashed {
        peer: PublicKey,
        path: PathBuf,
        result: Result<(u64, [u8; 32]), String>,
    },
    FileStream {
        peer: PublicKey,
        conn_id: usize,
        id: u64,
        recv: RecvStream,
    },
    FileProgress {
        peer: PublicKey,
        id: u64,
        incoming: bool,
        done: u64,
        total: u64,
    },
    FileSent {
        peer: PublicKey,
        id: u64,
        result: Result<(), String>,
    },
    FileReceived {
        peer: PublicKey,
        conn_id: usize,
        id: u64,
        result: Result<PathBuf, String>,
    },
}

struct OutgoingFile {
    path: PathBuf,
    size: u64,
}

struct Session {
    conn: Connection,
    out: mpsc::UnboundedSender<Frame>,
    unacked: HashSet<u64>,
    path: PathKind,
    tasks: Vec<JoinHandle<()>>,
    /// Files we offered, by id, until accepted/rejected.
    outgoing: HashMap<u64, OutgoingFile>,
    /// Files offered to us, waiting for the user's answer.
    offers: HashMap<u64, Offer>,
    /// Accepted files whose data stream has not arrived yet.
    expecting: HashMap<u64, (PathBuf, Offer)>,
    receiving: usize,
}

impl Session {
    fn id(&self) -> usize {
        self.conn.stable_id()
    }

    fn i_dialed(&self) -> bool {
        self.conn.side() == Side::Client
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct Engine {
    ep: Endpoint,
    shared: Arc<Shared>,
    codename: String,
    sink: Sink,
    sessions: HashMap<PublicKey, Session>,
    /// Peers being dialed, with whether the user asked for it explicitly.
    dialing: HashMap<PublicKey, bool>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
    auto_connect: Option<Duration>,
    pending_pairs: HashMap<PublicKey, (Connection, SendStream, String)>,
    pairing_was_open: bool,
    downloads: PathBuf,
    max_file_size: u64,
    next_file_id: u64,
}

impl Engine {
    fn new(ep: Endpoint, cfg: EngineConfig, shared: Arc<Shared>, sink: Sink) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        Engine {
            ep,
            shared,
            codename: cfg.codename,
            sink,
            sessions: HashMap::new(),
            dialing: HashMap::new(),
            internal_tx,
            internal_rx: Some(internal_rx),
            auto_connect: cfg.auto_connect,
            pending_pairs: HashMap::new(),
            pairing_was_open: false,
            downloads: cfg.downloads,
            max_file_size: cfg.max_file_size,
            next_file_id: 1,
        }
    }

    fn me(&self) -> PublicKey {
        self.ep.id()
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) {
        self.sink.emit(Event::Ready {
            addr: self.ep.addr(),
        });
        let mut internal = self.internal_rx.take().expect("run once");
        self.spawn_accept_loop();
        self.spawn_online_watch();

        let mut path_tick = tokio::time::interval(PATH_POLL);
        let auto = self.auto_connect.unwrap_or(Duration::from_secs(3600));
        let mut auto_tick = tokio::time::interval(auto);

        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Command::Shutdown { panic }) => {
                        self.shutdown(panic).await;
                        return;
                    }
                    None => {
                        self.shutdown(false).await;
                        return;
                    }
                    Some(cmd) => self.on_command(cmd),
                },
                Some(msg) = internal.recv() => self.on_internal(msg),
                _ = path_tick.tick() => {
                    self.poll_paths();
                    self.check_pairing_window();
                }
                _ = auto_tick.tick(), if self.auto_connect.is_some() => self.auto_dial(),
            }
        }
    }

    fn spawn_accept_loop(&self) {
        let ep = self.ep.clone();
        let tx = self.internal_tx.clone();
        let codename = self.codename.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let tx = tx.clone();
                let codename = codename.clone();
                tokio::spawn(async move {
                    // The Gate hook has already rejected disallowed peers here.
                    let Ok(conn) = incoming.await else { return };
                    if conn.alpn() == CHAT_ALPN {
                        if let Ok((send, recv, peer_name)) = hello_accept(&conn, &codename).await {
                            let _ = tx.send(Internal::Established {
                                conn,
                                send,
                                recv,
                                codename: peer_name,
                            });
                        }
                    } else if conn.alpn() == PAIR_ALPN {
                        if let Ok((send, codename)) = pair_accept(&conn).await {
                            let _ = tx.send(Internal::PairIncoming {
                                conn,
                                send,
                                codename,
                            });
                        }
                    } else {
                        conn.close(close::NOT_ALLOWED.into(), b"");
                    }
                });
            }
        });
    }

    fn spawn_online_watch(&self) {
        let ep = self.ep.clone();
        let events = self.sink.events.clone();
        let notify = self.sink.notify.clone();
        tokio::spawn(async move {
            ep.online().await;
            if events.send(Event::Network { online: true }).is_ok() {
                notify(false);
            }
        });
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::Connect(peer) => self.dial(peer, true),
            Command::Disconnect(peer) => {
                if let Some(s) = self.sessions.remove(&peer) {
                    let _ = s.out.send(Frame::Bye);
                    self.end_session(peer, s, close::NORMAL, None);
                }
            }
            Command::SendText { to, id, body } => self.send_text(to, id, body),
            Command::SetCodename(name) => {
                self.codename = name.clone();
                for s in self.sessions.values() {
                    let _ = s.out.send(Frame::Codename {
                        codename: name.clone(),
                    });
                }
            }
            Command::OpenPairing(d) => {
                *self.shared.pairing_until.lock().expect("pairing lock") = Some(Instant::now() + d);
                // Always confirm, also when re-opening an already open window.
                self.pairing_was_open = true;
                self.sink.emit(Event::PairingWindow { open: true });
            }
            Command::ClosePairing => {
                *self.shared.pairing_until.lock().expect("pairing lock") = None;
                self.check_pairing_window();
            }
            Command::Pair(key) => self.pair_dial(key),
            Command::ContactsUpdated => self.enforce_allowlist(),
            Command::SendFile { to, path } => self.offer_file(to, path),
            Command::FileDecision { peer, id, accept } => self.decide_file(peer, id, accept),
            Command::PairDecision { peer, accept } => self.pair_decide(peer, accept),
            Command::Shutdown { .. } => unreachable!("handled in run"),
        }
    }

    fn send_text(&mut self, to: PublicKey, id: u64, body: String) {
        let status = if body.len() > proto::MAX_TEXT_BYTES {
            Delivery::Failed(format!(
                "message too long (max {} KB)",
                proto::MAX_TEXT_BYTES / 1024
            ))
        } else if let Some(s) = self.sessions.get_mut(&to) {
            if s.out.send(Frame::Text { id, body }).is_ok() {
                s.unacked.insert(id);
                Delivery::Sent
            } else {
                Delivery::Failed("connection lost — not delivered".into())
            }
        } else {
            Delivery::Failed("contact is offline — nothing was queued".into())
        };
        self.sink.emit(Event::Delivery {
            peer: to,
            id,
            status,
        });
    }

    fn dial(&mut self, peer: PublicKey, manual: bool) {
        if self.sessions.contains_key(&peer) || self.dialing.contains_key(&peer) {
            return;
        }
        if !self.shared.allowed_chat(&peer) {
            if manual {
                self.sink.emit(Event::Warning(
                    "connection blocked: contact is not trusted (re-verify first)".into(),
                ));
            }
            return;
        }
        self.dialing.insert(peer, manual);
        self.sink.emit(Event::Presence {
            peer,
            presence: Presence::Connecting,
        });
        let ep = self.ep.clone();
        let tx = self.internal_tx.clone();
        let codename = self.codename.clone();
        tokio::spawn(async move {
            let res = async {
                let conn = timeout(DIAL_TIMEOUT, ep.connect(peer, CHAT_ALPN))
                    .await
                    .map_err(|_| "timed out".to_string())?
                    .map_err(|e| e.to_string())?;
                let (send, recv, name) = hello_dial(&conn, &codename).await?;
                Ok::<_, String>((conn, send, recv, name))
            }
            .await;
            let msg = match res {
                Ok((conn, send, recv, codename)) => Internal::Established {
                    conn,
                    send,
                    recv,
                    codename,
                },
                Err(reason) => Internal::DialFailed { peer, reason },
            };
            let _ = tx.send(msg);
        });
    }

    /// Closes sessions with peers that are no longer allowed (removed, or
    /// replaced by a changed key).
    fn enforce_allowlist(&mut self) {
        let blocked: Vec<PublicKey> = self
            .sessions
            .keys()
            .filter(|k| !self.shared.allowed_chat(k))
            .copied()
            .collect();
        for peer in blocked {
            if let Some(s) = self.sessions.remove(&peer) {
                self.end_session(
                    peer,
                    s,
                    close::NOT_ALLOWED,
                    Some("contact no longer trusted".into()),
                );
            }
        }
    }

    fn offer_file(&mut self, to: PublicKey, path: PathBuf) {
        if !self.sessions.contains_key(&to) {
            self.sink.emit(Event::Warning(format!(
                "{} is offline — files can only be sent while both of you are online",
                self.name(&to)
            )));
            return;
        }
        let tx = self.internal_tx.clone();
        let max = self.max_file_size;
        tokio::spawn(async move {
            let p = path.clone();
            let result = tokio::task::spawn_blocking(move || transfer::hash_file(&p, max))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = tx.send(Internal::FileHashed {
                peer: to,
                path,
                result,
            });
        });
    }

    fn on_file_hashed(
        &mut self,
        peer: PublicKey,
        path: PathBuf,
        result: Result<(u64, [u8; 32]), String>,
    ) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (size, hash) = match result {
            Ok(v) => v,
            Err(e) => {
                self.sink
                    .emit(Event::Warning(format!("cannot send {name}: {e}")));
                return;
            }
        };
        let name = match sanitize::filename(&name) {
            Ok(n) => n,
            Err(e) => {
                self.sink
                    .emit(Event::Warning(format!("cannot send this file: {e}")));
                return;
            }
        };
        let id = self.next_file_id;
        self.next_file_id += 1;
        let Some(s) = self.sessions.get_mut(&peer) else {
            self.sink.emit(Event::Warning(
                "contact went offline before the file could be offered".into(),
            ));
            return;
        };
        s.outgoing.insert(id, OutgoingFile { path, size });
        let _ = s.out.send(Frame::FileOffer {
            id,
            name: name.clone(),
            size,
            hash,
        });
        self.sink.emit(Event::FileOffered {
            peer,
            id,
            name,
            size,
        });
    }

    fn decide_file(&mut self, peer: PublicKey, id: u64, accept: bool) {
        let Some(s) = self.sessions.get_mut(&peer) else {
            self.sink.emit(Event::Warning(
                "contact is offline — the file offer has expired".into(),
            ));
            return;
        };
        let Some(offer) = s.offers.remove(&id) else {
            self.sink.emit(Event::Warning("no such file offer".into()));
            return;
        };
        if !accept {
            let _ = s.out.send(Frame::FileReject { id });
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.downloads) {
            let _ = s.out.send(Frame::FileReject { id });
            self.sink.emit(Event::Warning(format!(
                "cannot create download folder {}: {e}",
                self.downloads.display()
            )));
            return;
        }
        let dest = transfer::unique_path(&self.downloads, &offer.name);
        // Reserve the name so a second accept cannot pick the same path.
        if let Err(e) = std::fs::File::create(transfer::part_path(&dest)) {
            let _ = s.out.send(Frame::FileReject { id });
            self.sink
                .emit(Event::Warning(format!("cannot save file: {e}")));
            return;
        }
        s.expecting.insert(id, (dest, offer));
        let _ = s.out.send(Frame::FileAccept { id });
    }

    fn on_file_offer(&mut self, peer: PublicKey, id: u64, name: String, size: u64, hash: [u8; 32]) {
        let max = self.max_file_size;
        let who = self.name(&peer);
        let Some(s) = self.sessions.get_mut(&peer) else {
            return;
        };
        let pending = s.offers.len() + s.expecting.len() + s.receiving;
        let reject = |s: &mut Session| {
            let _ = s.out.send(Frame::FileReject { id });
        };
        if pending >= MAX_PENDING_FILES
            || s.offers.contains_key(&id)
            || s.expecting.contains_key(&id)
        {
            reject(s);
            return;
        }
        let name = match sanitize::filename(&name) {
            Ok(n) => n,
            Err(e) => {
                reject(s);
                self.sink.emit(Event::Warning(format!(
                    "{who} offered a file with an unsafe name ({e}); rejected automatically"
                )));
                return;
            }
        };
        if size > max {
            reject(s);
            self.sink.emit(Event::Notice(format!(
                "{who} tried to send {name} ({}), above your limit of {}; rejected automatically",
                transfer::human_size(size),
                transfer::human_size(max)
            )));
            return;
        }
        let executable = sanitize::is_executable(&name);
        s.offers.insert(
            id,
            Offer {
                name: name.clone(),
                size,
                hash,
            },
        );
        self.sink.emit(Event::FileIncoming {
            peer,
            id,
            name,
            size,
            executable,
        });
    }

    fn on_file_accepted(&mut self, peer: PublicKey, id: u64) {
        let Some(s) = self.sessions.get_mut(&peer) else {
            return;
        };
        let Some(file) = s.outgoing.remove(&id) else {
            return;
        };
        let conn = s.conn.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let total = file.size;
            let ptx = tx.clone();
            let result = transfer::send(&conn, id, &file.path, |done| {
                let _ = ptx.send(Internal::FileProgress {
                    peer,
                    id,
                    incoming: false,
                    done,
                    total,
                });
            })
            .await;
            let _ = tx.send(Internal::FileSent { peer, id, result });
        });
    }

    fn on_file_stream(&mut self, peer: PublicKey, conn_id: usize, id: u64, mut recv: RecvStream) {
        let Some(s) = self.sessions.get_mut(&peer).filter(|s| s.id() == conn_id) else {
            let _ = recv.stop(close::NOT_ALLOWED.into());
            return;
        };
        // Only streams for files the user accepted are read.
        let Some((dest, offer)) = s.expecting.remove(&id) else {
            let _ = recv.stop(close::PROTOCOL_ERROR.into());
            return;
        };
        s.receiving += 1;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let total = offer.size;
            let ptx = tx.clone();
            let result = transfer::receive(&mut recv, &dest, &offer, |done| {
                let _ = ptx.send(Internal::FileProgress {
                    peer,
                    id,
                    incoming: true,
                    done,
                    total,
                });
            })
            .await
            .map(|()| dest);
            let _ = tx.send(Internal::FileReceived {
                peer,
                conn_id,
                id,
                result,
            });
        });
    }

    fn check_pairing_window(&mut self) {
        let open = self.shared.pairing_open();
        if open != self.pairing_was_open {
            self.pairing_was_open = open;
            self.sink.emit(Event::PairingWindow { open });
        }
    }

    fn pair_dial(&mut self, ck: ContactKey) {
        if ck.key == self.me() {
            self.sink
                .emit(Event::Warning("that is your own Contact Key".into()));
            return;
        }
        let ep = self.ep.clone();
        let tx = self.internal_tx.clone();
        let codename = self.codename.clone();
        self.sink.emit(Event::Notice(format!(
            "PAIR REQUEST -> {}: connecting...",
            ck.codename
        )));
        tokio::spawn(async move {
            let result = async {
                let conn = timeout(DIAL_TIMEOUT, ep.connect(ck.key, PAIR_ALPN))
                    .await
                    .map_err(|_| "timed out".to_string())?
                    .map_err(|e| e.to_string())?;
                let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
                proto::write_frame(
                    &mut send,
                    &PairFrame::Request {
                        version: PROTOCOL_VERSION,
                        codename,
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
                let answer = timeout(
                    PAIR_RESPONSE_TIMEOUT,
                    proto::read_frame::<_, PairFrame>(&mut recv),
                )
                .await
                .map_err(|_| "no answer, the request expired".to_string())?
                .map_err(|e| match conn.close_reason() {
                    Some(reason) => describe_close(&reason),
                    None => e.to_string(),
                })?;
                conn.close(close::NORMAL.into(), b"");
                match answer {
                    PairFrame::Response {
                        accepted: true,
                        codename,
                    } => Ok(codename::validate(&codename).ok()),
                    PairFrame::Response {
                        accepted: false, ..
                    } => Err("the request was declined".to_string()),
                    PairFrame::Request { .. } => Err("protocol error".to_string()),
                }
            }
            .await;
            let _ = tx.send(Internal::PairAnswered { key: ck, result });
        });
    }

    fn pair_decide(&mut self, peer: PublicKey, accept: bool) {
        let Some((conn, mut send, codename)) = self.pending_pairs.remove(&peer) else {
            self.sink.emit(Event::Warning(
                "no pending pair request from that contact".into(),
            ));
            return;
        };
        let reply = PairFrame::Response {
            accepted: accept,
            codename: if accept {
                self.codename.clone()
            } else {
                String::new()
            },
        };
        if accept {
            let outcome = self
                .shared
                .contacts
                .lock()
                .expect("contacts lock")
                .add(peer, codename);
            self.sink.emit(Event::ContactsChanged);
            self.sink.emit(Event::Paired { peer, outcome });
            self.enforce_allowlist();
        }
        tokio::spawn(async move {
            if proto::write_frame(&mut send, &reply).await.is_ok() {
                let _ = send.finish();
                // Let the requester read the answer and close first.
                let _ = timeout(Duration::from_secs(5), conn.closed()).await;
            }
            conn.close(close::NORMAL.into(), b"");
        });
    }

    fn on_pair_answered(&mut self, ck: ContactKey, result: Result<Option<String>, String>) {
        match result {
            Ok(codename) => {
                let peer = ck.key;
                let outcome = self
                    .shared
                    .contacts
                    .lock()
                    .expect("contacts lock")
                    .add(peer, codename.unwrap_or(ck.codename));
                self.sink.emit(Event::ContactsChanged);
                self.sink.emit(Event::Paired { peer, outcome });
                self.enforce_allowlist();
                // Open the chat channel straight away (blocked if the key changed).
                self.dial(peer, false);
            }
            Err(reason) => self.sink.emit(Event::Warning(format!(
                "pairing with {} failed: {} (is their /invite window open?)",
                ck.codename,
                sanitize::text(&reason)
            ))),
        }
    }

    fn auto_dial(&mut self) {
        let peers: Vec<PublicKey> = {
            let book = self.shared.contacts.lock().expect("contacts lock");
            book.iter()
                .filter(|c| book.can_connect(&c.key))
                .map(|c| c.key)
                .collect()
        };
        for p in peers {
            self.dial(p, false);
        }
    }

    fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Established {
                conn,
                send,
                recv,
                codename,
            } => self.on_established(conn, send, recv, codename),
            Internal::DialFailed { peer, reason } => {
                let manual = self.dialing.remove(&peer).unwrap_or(false);
                if self.sessions.contains_key(&peer) {
                    return;
                }
                self.sink.emit(Event::Presence {
                    peer,
                    presence: Presence::Offline,
                });
                if manual {
                    let name = self.name(&peer);
                    self.sink.emit(Event::Notice(format!(
                        "{name} is not reachable ({})",
                        sanitize::text(&reason)
                    )));
                }
            }
            Internal::Frame {
                peer,
                conn_id,
                frame,
            } => {
                if self.sessions.get(&peer).is_some_and(|s| s.id() == conn_id) {
                    self.on_frame(peer, frame);
                }
            }
            Internal::PairIncoming {
                conn,
                send,
                codename,
            } => {
                let peer = conn.remote_id();
                if self.pending_pairs.len() >= MAX_PENDING_PAIRS
                    && !self.pending_pairs.contains_key(&peer)
                {
                    conn.close(close::NOT_ALLOWED.into(), b"");
                    return;
                }
                let outcome = self
                    .shared
                    .contacts
                    .lock()
                    .expect("contacts lock")
                    .classify(&peer, &codename);
                if let Some((old, ..)) = self
                    .pending_pairs
                    .insert(peer, (conn, send, codename.clone()))
                {
                    old.close(close::DUPLICATE.into(), b"");
                }
                self.sink.emit(Event::PairRequest {
                    peer,
                    codename,
                    outcome,
                });
            }
            Internal::PairAnswered { key, result } => self.on_pair_answered(key, result),
            Internal::FileHashed { peer, path, result } => self.on_file_hashed(peer, path, result),
            Internal::FileStream {
                peer,
                conn_id,
                id,
                recv,
            } => self.on_file_stream(peer, conn_id, id, recv),
            Internal::FileProgress {
                peer,
                id,
                incoming,
                done,
                total,
            } => self.sink.emit(Event::FileProgress {
                peer,
                id,
                incoming,
                done,
                total,
            }),
            Internal::FileSent { peer, id, result } => {
                // Success is reported when the receiver confirms (FileResult).
                if let Err(e) = result {
                    self.sink.emit(Event::FileDone {
                        peer,
                        id,
                        incoming: false,
                        result: Err(e),
                    });
                }
            }
            Internal::FileReceived {
                peer,
                conn_id,
                id,
                result,
            } => {
                if let Some(s) = self.sessions.get_mut(&peer).filter(|s| s.id() == conn_id) {
                    s.receiving = s.receiving.saturating_sub(1);
                    let _ = s.out.send(Frame::FileResult {
                        id,
                        ok: result.is_ok(),
                    });
                }
                self.sink.emit(Event::FileDone {
                    peer,
                    id,
                    incoming: true,
                    result: result.map(Some),
                });
            }
            Internal::Ended {
                peer,
                conn_id,
                reason,
            } => {
                if self.sessions.get(&peer).is_some_and(|s| s.id() == conn_id) {
                    let s = self.sessions.remove(&peer).expect("checked");
                    self.end_session(peer, s, close::NORMAL, Some(reason));
                }
            }
        }
    }

    fn on_established(
        &mut self,
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        codename: Option<String>,
    ) {
        let peer = conn.remote_id();
        self.dialing.remove(&peer);
        // Contact may have been removed / blocked while we were connecting.
        if !self.shared.allowed_chat(&peer) {
            conn.close(close::NOT_ALLOWED.into(), b"");
            return;
        }
        if let Some(old) = self.sessions.get(&peer) {
            let me = self.me();
            let dialer = |i_dialed: bool| if i_dialed { me } else { peer };
            let old_dialer = dialer(old.i_dialed());
            let new_dialer = dialer(conn.side() == Side::Client);
            // Both sides apply the same rule, so they keep the same connection.
            let keep_new = old_dialer == new_dialer || new_dialer < old_dialer;
            if !keep_new {
                conn.close(close::DUPLICATE.into(), b"");
                return;
            }
            let old = self.sessions.remove(&peer).expect("checked");
            self.fail_unacked(peer, &old);
            old.conn.close(close::DUPLICATE.into(), b"");
        }

        if let Some(name) = codename {
            let old = self
                .shared
                .contacts
                .lock()
                .expect("contacts lock")
                .update_codename(&peer, name.clone());
            if let Some(old) = old {
                self.sink.emit(Event::CodenameChanged {
                    peer,
                    old,
                    new: name,
                });
                self.sink.emit(Event::ContactsChanged);
            }
        }

        let session = self.start_session(peer, conn, send, recv);
        self.sessions.insert(peer, session);
        self.sink.emit(Event::Handshake { peer });
        self.sink.emit(Event::Presence {
            peer,
            presence: Presence::Online,
        });
        self.poll_paths();
    }

    fn start_session(
        &self,
        peer: PublicKey,
        conn: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Session {
        let conn_id = conn.stable_id();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Frame>();
        let tx = self.internal_tx.clone();
        let reader = tokio::spawn(async move {
            let reason = loop {
                match proto::read_frame::<_, Frame>(&mut recv).await {
                    Ok(frame) => {
                        if tx
                            .send(Internal::Frame {
                                peer,
                                conn_id,
                                frame,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(proto::ProtoError::Closed) => break "disconnected".to_string(),
                    Err(e @ (proto::ProtoError::Decode(_) | proto::ProtoError::TooLarge(_))) => {
                        break format!("protocol error: {e}");
                    }
                    Err(e) => break e.to_string(),
                }
            };
            let _ = tx.send(Internal::Ended {
                peer,
                conn_id,
                reason,
            });
        });
        let uni_conn = conn.clone();
        let uni_tx = self.internal_tx.clone();
        let streams = tokio::spawn(async move {
            while let Ok(mut recv) = uni_conn.accept_uni().await {
                let mut id = [0u8; 8];
                let ok = timeout(HELLO_TIMEOUT, recv.read_exact(&mut id)).await;
                if !matches!(ok, Ok(Ok(()))) {
                    let _ = recv.stop(close::PROTOCOL_ERROR.into());
                    continue;
                }
                let msg = Internal::FileStream {
                    peer,
                    conn_id,
                    id: u64::from_be_bytes(id),
                    recv,
                };
                if uni_tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let writer = tokio::spawn(async move {
            while let Some(mut frame) = out_rx.recv().await {
                let bye = frame == Frame::Bye;
                let res = proto::write_frame(&mut send, &frame).await;
                if let Frame::Text { body, .. } = &mut frame {
                    body.zeroize();
                }
                if res.is_err() {
                    break;
                }
                if bye {
                    break;
                }
            }
            let _ = send.finish();
        });
        Session {
            conn,
            out,
            unacked: HashSet::new(),
            path: PathKind::Unknown,
            tasks: vec![reader, writer, streams],
            outgoing: HashMap::new(),
            offers: HashMap::new(),
            expecting: HashMap::new(),
            receiving: 0,
        }
    }

    fn on_frame(&mut self, peer: PublicKey, frame: Frame) {
        match frame {
            Frame::Text { id, body } => {
                if let Some(s) = self.sessions.get(&peer) {
                    let _ = s.out.send(Frame::Ack { id });
                }
                self.sink.emit(Event::Text {
                    peer,
                    body: sanitize::text(&body),
                });
            }
            Frame::Ack { id } => {
                if self
                    .sessions
                    .get_mut(&peer)
                    .is_some_and(|s| s.unacked.remove(&id))
                {
                    self.sink.emit(Event::Delivery {
                        peer,
                        id,
                        status: Delivery::Delivered,
                    });
                }
            }
            Frame::Codename { codename } => {
                let Ok(name) = codename::validate(&codename) else {
                    return;
                };
                let old = self
                    .shared
                    .contacts
                    .lock()
                    .expect("contacts lock")
                    .update_codename(&peer, name.clone());
                if let Some(old) = old {
                    self.sink.emit(Event::CodenameChanged {
                        peer,
                        old,
                        new: name,
                    });
                    self.sink.emit(Event::ContactsChanged);
                }
            }
            Frame::Bye => {
                if let Some(s) = self.sessions.remove(&peer) {
                    self.end_session(peer, s, close::NORMAL, Some("contact left".into()));
                }
            }
            // A second Hello is a protocol violation.
            Frame::Hello { .. } => {
                if let Some(s) = self.sessions.remove(&peer) {
                    self.end_session(
                        peer,
                        s,
                        close::PROTOCOL_ERROR,
                        Some("protocol error".into()),
                    );
                }
            }
            Frame::FileOffer {
                id,
                name,
                size,
                hash,
            } => self.on_file_offer(peer, id, name, size, hash),
            Frame::FileAccept { id } => self.on_file_accepted(peer, id),
            Frame::FileReject { id } => {
                if let Some(s) = self.sessions.get_mut(&peer)
                    && s.outgoing.remove(&id).is_some()
                {
                    self.sink.emit(Event::FileDone {
                        peer,
                        id,
                        incoming: false,
                        result: Err("declined by the contact".into()),
                    });
                }
            }
            Frame::FileResult { id, ok } => self.sink.emit(Event::FileDone {
                peer,
                id,
                incoming: false,
                result: if ok {
                    Ok(None)
                } else {
                    Err("the contact's copy failed the integrity check and was deleted".into())
                },
            }),
        }
    }

    fn fail_unacked(&self, peer: PublicKey, s: &Session) {
        for id in &s.unacked {
            self.sink.emit(Event::Delivery {
                peer,
                id: *id,
                status: Delivery::Failed("connection lost before delivery".into()),
            });
        }
    }

    fn end_session(&mut self, peer: PublicKey, s: Session, code: u32, reason: Option<String>) {
        self.fail_unacked(peer, &s);
        // Accepted files whose data never started: drop the reserved names.
        for (dest, _) in s.expecting.values() {
            let _ = std::fs::remove_file(transfer::part_path(dest));
        }
        s.conn.close(code.into(), b"");
        drop(s);
        self.sink.emit(Event::Presence {
            peer,
            presence: Presence::Offline,
        });
        if let Some(r) = reason {
            let name = self.name(&peer);
            self.sink.emit(Event::Notice(format!(
                "{name}: secure channel closed ({r})"
            )));
        }
    }

    fn poll_paths(&mut self) {
        let mut changes = Vec::new();
        for (peer, s) in self.sessions.iter_mut() {
            let path = path_kind(&s.conn);
            if path != s.path {
                s.path = path;
                changes.push((*peer, path));
            }
        }
        for (peer, path) in changes {
            self.sink.emit(Event::Path { peer, path });
        }
    }

    fn name(&self, peer: &PublicKey) -> String {
        self.shared
            .contacts
            .lock()
            .expect("contacts lock")
            .display_name(peer)
    }

    async fn shutdown(&mut self, panic: bool) {
        let code = if panic { close::PANIC } else { close::NORMAL };
        for (_, (conn, ..)) in self.pending_pairs.drain() {
            conn.close(code.into(), b"");
        }
        for (_, s) in self.sessions.drain() {
            let _ = s.out.send(Frame::Bye);
            s.conn.close(code.into(), b"");
        }
        // Give close frames a moment to leave, but never block a panic exit.
        let wait = if panic {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(2)
        };
        let _ = timeout(wait, self.ep.close()).await;
        self.sink.emit(Event::Stopped);
    }
}

fn path_kind(conn: &Connection) -> PathKind {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) if p.is_relay() => PathKind::Relay,
        Some(p) if p.is_ip() => PathKind::Direct,
        _ => PathKind::Unknown,
    }
}

/// Validates the peer's Hello. Returns its codename if it is well-formed.
fn check_hello(conn: &Connection, frame: Frame) -> Result<Option<String>, String> {
    match frame {
        Frame::Hello { version, codename } if version == PROTOCOL_VERSION => {
            Ok(codename::validate(&codename).ok())
        }
        Frame::Hello { version, .. } => {
            conn.close(close::PROTOCOL_VERSION.into(), b"");
            Err(format!(
                "peer speaks protocol v{version}, this app speaks v{PROTOCOL_VERSION} — one of you needs to update"
            ))
        }
        _ => {
            conn.close(close::PROTOCOL_ERROR.into(), b"");
            Err("protocol error during handshake".into())
        }
    }
}

async fn hello_dial(
    conn: &Connection,
    my_codename: &str,
) -> Result<(SendStream, RecvStream, Option<String>), String> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    proto::write_frame(
        &mut send,
        &Frame::Hello {
            version: PROTOCOL_VERSION,
            codename: my_codename.to_string(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let frame = timeout(HELLO_TIMEOUT, proto::read_frame::<_, Frame>(&mut recv))
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| match conn.close_reason() {
            Some(reason) => describe_close(&reason),
            None => e.to_string(),
        })?;
    let name = check_hello(conn, frame)?;
    Ok((send, recv, name))
}

/// Reads and validates an incoming pair request. Invalid requests are
/// dropped without an answer.
async fn pair_accept(conn: &Connection) -> Result<(SendStream, String), ()> {
    let (send, mut recv) = timeout(HELLO_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    let frame = timeout(HELLO_TIMEOUT, proto::read_frame::<_, PairFrame>(&mut recv))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    let code = match frame {
        PairFrame::Request { version, codename } if version == PROTOCOL_VERSION => {
            match codename::validate(&codename) {
                Ok(name) => return Ok((send, name)),
                Err(_) => close::PROTOCOL_ERROR,
            }
        }
        PairFrame::Request { .. } => close::PROTOCOL_VERSION,
        PairFrame::Response { .. } => close::PROTOCOL_ERROR,
    };
    conn.close(code.into(), b"");
    Err(())
}

async fn hello_accept(
    conn: &Connection,
    my_codename: &str,
) -> Result<(SendStream, RecvStream, Option<String>), String> {
    let (mut send, mut recv) = timeout(HELLO_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let frame = timeout(HELLO_TIMEOUT, proto::read_frame::<_, Frame>(&mut recv))
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let name = check_hello(conn, frame)?;
    proto::write_frame(
        &mut send,
        &Frame::Hello {
            version: PROTOCOL_VERSION,
            codename: my_codename.to_string(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok((send, recv, name))
}

fn describe_close(reason: &iroh::endpoint::ConnectionError) -> String {
    use iroh::endpoint::ConnectionError;
    match reason {
        ConnectionError::ApplicationClosed(c) => match c.error_code.into_inner() as u32 {
            close::NOT_ALLOWED => {
                "refused: you are not in their contacts (or they blocked a key change)".into()
            }
            close::PROTOCOL_VERSION => "incompatible app version".into(),
            close::DUPLICATE => "duplicate connection".into(),
            _ => "closed by peer".into(),
        },
        other => other.to_string(),
    }
}
