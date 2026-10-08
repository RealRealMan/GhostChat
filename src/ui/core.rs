//! Main-screen state: conversations, contacts panel, command handling.
//! Exists only after the identity is unlocked.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};
use iroh::PublicKey;
use tokio::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

use super::{
    commands::{self, Cmd},
    theme::{Palette, ThemeName, mono},
};
use crate::{
    codename,
    contact_key::{self, ContactKey},
    contacts::{AddOutcome, ContactBook, Trust},
    engine::{Command, Delivery, EngineHandle, Event, PathKind, Presence},
    safety, sanitize,
    storage::{self, Profile, Settings},
    transfer::human_size,
};

const ALIAS_MAX: usize = 24;

/// One line in a conversation or the console. Zeroized on drop (PRD §9).
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    System,
    Ok,
    Warn,
    Alert,
    /// Pre-formatted block (help, keys, randomart).
    Block,
    Mine {
        id: u64,
        status: Delivery,
    },
    Theirs,
}

impl Drop for Line {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}

pub struct PairReq {
    pub peer: PublicKey,
    pub codename: String,
    pub outcome: AddOutcome,
}

pub struct FileReq {
    pub peer: PublicKey,
    pub id: u64,
    pub name: String,
}

/// An active transfer shown with a progress bar.
pub struct Transfer {
    pub name: String,
    pub total: u64,
    pub done: u64,
}

pub enum Confirm {
    AddKeyChange(ContactKey),
    Remove(PublicKey),
}

/// What a handled command asks the app to do next.
pub enum Action {
    None,
    Verify(PublicKey),
    Panic,
    Quit,
}

pub struct Core {
    pub engine: EngineHandle,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub contacts: Arc<Mutex<ContactBook>>,
    pub me: PublicKey,
    pub storage_key: Zeroizing<[u8; 32]>,
    /// `None` = the console.
    pub convos: HashMap<Option<PublicKey>, Vec<Line>>,
    pub selected: Option<PublicKey>,
    pub unread: HashSet<PublicKey>,
    pub presence: HashMap<PublicKey, Presence>,
    pub paths: HashMap<PublicKey, PathKind>,
    pub net_online: bool,
    pub pairing_until: Option<Instant>,
    pub pair_requests: Vec<PairReq>,
    pub confirm: Option<Confirm>,
    pub input: String,
    pub next_id: u64,
    pub alert_until: Option<Instant>,
    /// Peers whose secure channel was just established (handshake animation).
    pub handshakes: Vec<(PublicKey, Instant)>,
    /// Incoming file offers waiting for /accept or /reject.
    pub file_requests: Vec<FileReq>,
    /// Keyed by (peer, id, incoming).
    pub transfers: HashMap<(PublicKey, u64, bool), Transfer>,
}

impl Core {
    pub fn new(
        engine: EngineHandle,
        events: mpsc::UnboundedReceiver<Event>,
        contacts: Arc<Mutex<ContactBook>>,
        me: PublicKey,
        storage_key: Zeroizing<[u8; 32]>,
    ) -> Self {
        Core {
            engine,
            events,
            contacts,
            me,
            storage_key,
            convos: HashMap::new(),
            selected: None,
            unread: HashSet::new(),
            presence: HashMap::new(),
            paths: HashMap::new(),
            net_online: false,
            pairing_until: None,
            pair_requests: Vec::new(),
            confirm: None,
            input: String::new(),
            next_id: 1,
            alert_until: None,
            handshakes: Vec::new(),
            file_requests: Vec::new(),
            transfers: HashMap::new(),
        }
    }

    pub fn name(&self, key: &PublicKey) -> String {
        self.contacts.lock().expect("contacts").display_name(key)
    }

    pub fn trust(&self, key: &PublicKey) -> Option<Trust> {
        self.contacts
            .lock()
            .expect("contacts")
            .get(key)
            .map(|c| c.trust)
    }

    pub fn push(&mut self, convo: Option<PublicKey>, kind: LineKind, text: impl Into<String>) {
        if let Some(k) = convo
            && self.selected != Some(k)
            && kind == LineKind::Theirs
        {
            self.unread.insert(k);
        }
        self.convos.entry(convo).or_default().push(Line {
            kind,
            text: text.into(),
        });
    }

    pub fn console(&mut self, kind: LineKind, text: impl Into<String>) {
        self.push(None, kind, text);
    }

    /// Feedback for a command goes where the user is looking.
    fn reply(&mut self, kind: LineKind, text: impl Into<String>) {
        let convo = self.selected;
        self.push(convo, kind, text);
    }

    pub fn save_contacts(&mut self, profile: &Profile) {
        let res = {
            let book = self.contacts.lock().expect("contacts");
            storage::save_contacts(&profile.contacts_path(), &self.storage_key, &book)
        };
        if let Err(e) = res {
            self.console(LineKind::Warn, format!("could not save contacts: {e}"));
        }
    }

    fn resolve(&mut self, name: Option<&str>) -> Option<PublicKey> {
        match name {
            None => {
                if self.selected.is_none() {
                    self.reply(
                        LineKind::Warn,
                        "which contact? add a name, or select one on the left",
                    );
                }
                self.selected
            }
            Some(n) => {
                let found = self.contacts.lock().expect("contacts").find(n);
                if found.is_none() {
                    self.reply(
                        LineKind::Warn,
                        format!(
                            "no single contact called {} — see /contacts",
                            sanitize::text(n)
                        ),
                    );
                }
                found
            }
        }
    }

    fn my_contact_key(&self, settings: &Settings) -> ContactKey {
        my_key(self.me, settings)
    }

    pub fn visible_lines(&self) -> Vec<String> {
        let peer_name = self.selected.map(|k| self.name(&k)).unwrap_or_default();
        self.convos
            .get(&self.selected)
            .into_iter()
            .flatten()
            .map(|l| match l.kind {
                LineKind::Theirs => format!("{peer_name}> {}", l.text),
                LineKind::Mine { .. } => format!("YOU> {}", l.text),
                _ => l.text.clone(),
            })
            .collect()
    }

    /// Drops every piece of conversation state from memory (zeroized).
    pub fn wipe_memory(&mut self) {
        self.convos.clear();
        self.input.zeroize();
        self.pair_requests.clear();
        self.confirm = None;
        self.unread.clear();
        self.file_requests.clear();
        self.transfers.clear();
    }

    pub fn drain_events(&mut self, profile: &Profile) {
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev, profile);
        }
    }

    fn on_event(&mut self, ev: Event, profile: &Profile) {
        match ev {
            Event::Ready { addr } => self.console(
                LineKind::Ok,
                format!("[ OK ] Node {} listening", addr.id.fmt_short()),
            ),
            Event::BindFailed(e) => self.console(LineKind::Alert, format!("[FAIL] {e}")),
            Event::Network { online } => {
                self.net_online = online;
                if online {
                    self.console(LineKind::Ok, "[ OK ] Connected to relay network");
                }
            }
            Event::Presence { peer, presence } => {
                self.presence.insert(peer, presence);
                if presence != Presence::Online {
                    self.paths.remove(&peer);
                }
            }
            Event::Path { peer, path } => {
                self.paths.insert(peer, path);
            }
            Event::Handshake { peer } => {
                self.handshakes.push((peer, Instant::now()));
                let name = self.name(&peer);
                self.push(
                    Some(peer),
                    LineKind::Ok,
                    format!("SECURE CHANNEL ESTABLISHED with {name}"),
                );
            }
            Event::Text { peer, body } => self.push(Some(peer), LineKind::Theirs, body),
            Event::Delivery { peer, id, status } => {
                if let Some(line) = self.convos.get_mut(&Some(peer)).and_then(|l| {
                    l.iter_mut()
                        .rev()
                        .find(|l| matches!(&l.kind, LineKind::Mine { id: i, .. } if *i == id))
                }) {
                    line.kind = LineKind::Mine {
                        id,
                        status: status.clone(),
                    };
                }
                if let Delivery::Failed(reason) = status {
                    self.push(
                        Some(peer),
                        LineKind::Warn,
                        format!("not delivered: {reason}"),
                    );
                }
            }
            Event::CodenameChanged { peer, old, new } => {
                self.push(
                    Some(peer),
                    LineKind::System,
                    format!("{old} is now known as {new}"),
                );
                self.save_contacts(profile);
            }
            Event::ContactsChanged => self.save_contacts(profile),
            Event::PairingWindow { open } => {
                if !open && self.pairing_until.take().is_some() {
                    self.console(LineKind::System, "Pairing window closed.");
                }
            }
            Event::PairRequest {
                peer,
                codename,
                outcome,
            } => {
                let key_change = matches!(outcome, AddOutcome::KeyChanged { .. });
                let mut msg = format!("INCOMING PAIR REQUEST: {codename}  —  /accept or /reject");
                if key_change {
                    self.alert_until = Some(Instant::now() + Duration::from_secs(2));
                    msg = format!(
                        "INTRUSION WARNING: KEY MISMATCH — {codename} is already a VERIFIED contact with a DIFFERENT key.\nOnly accept if {codename} told you by voice that they reinstalled GhostChat.\n{msg}"
                    );
                }
                self.console(
                    if key_change {
                        LineKind::Alert
                    } else {
                        LineKind::Warn
                    },
                    msg,
                );
                self.pair_requests.retain(|r| r.peer != peer);
                self.pair_requests.push(PairReq {
                    peer,
                    codename,
                    outcome,
                });
            }
            Event::Paired { peer, outcome } => {
                let name = self.name(&peer);
                if matches!(outcome, AddOutcome::KeyChanged { .. }) {
                    self.alert_until = Some(Instant::now() + Duration::from_secs(2));
                    self.console(
                        LineKind::Alert,
                        format!("{name}: KEY CHANGED — chat is blocked until you /verify {name}"),
                    );
                } else {
                    self.console(
                        LineKind::Ok,
                        format!(
                            "[ OK ] {name} added (UNVERIFIED). Call them by voice/video and run /verify {name}"
                        ),
                    );
                }
            }
            Event::FileOffered {
                peer,
                id,
                name,
                size,
            } => {
                let who = self.name(&peer);
                self.push(
                    Some(peer),
                    LineKind::System,
                    format!(
                        "OFFERED {name} ({}) — waiting for {who} to accept",
                        human_size(size)
                    ),
                );
                self.transfers.insert(
                    (peer, id, false),
                    Transfer {
                        name,
                        total: size,
                        done: 0,
                    },
                );
            }
            Event::FileIncoming {
                peer,
                id,
                name,
                size,
                executable,
            } => {
                let who = self.name(&peer);
                self.push(
                    Some(peer),
                    LineKind::Warn,
                    format!(
                        "INCOMING FILE from {who}: {name} ({}) — /accept to save it, /reject to refuse",
                        human_size(size)
                    ),
                );
                if executable {
                    self.alert_until = Some(Instant::now() + Duration::from_millis(800));
                    self.push(
                        Some(peer),
                        LineKind::Alert,
                        format!(
                            "{name} is a PROGRAM or SCRIPT. It can take over your computer if opened.
Only accept it if {who} told you (by voice) to expect it."
                        ),
                    );
                }
                if self.selected != Some(peer) {
                    self.unread.insert(peer);
                }
                self.file_requests.push(FileReq { peer, id, name });
            }
            Event::FileProgress {
                peer,
                id,
                incoming,
                done,
                total,
            } => {
                if let Some(t) = self.transfers.get_mut(&(peer, id, incoming)) {
                    t.done = done;
                    t.total = total;
                }
            }
            Event::FileDone {
                peer,
                id,
                incoming,
                result,
            } => {
                let name = self
                    .transfers
                    .remove(&(peer, id, incoming))
                    .map(|t| t.name)
                    .unwrap_or_else(|| "file".into());
                self.file_requests
                    .retain(|r| !(r.peer == peer && r.id == id));
                let who = self.name(&peer);
                let (kind, text) = match (incoming, result) {
                    (true, Ok(path)) => (
                        LineKind::Ok,
                        format!(
                            "[ OK ] Received {name} (integrity verified). Saved to:
  {}
  It is now a normal file on your disk; GhostChat no longer protects or deletes it.",
                            path.map(|p| p.display().to_string()).unwrap_or_default()
                        ),
                    ),
                    (false, Ok(_)) => (
                        LineKind::Ok,
                        format!("[ OK ] {name} delivered — {who} confirmed the integrity check"),
                    ),
                    (true, Err(e)) => (
                        LineKind::Warn,
                        format!("receiving {name} failed: {e}. Nothing was kept."),
                    ),
                    (false, Err(e)) => (LineKind::Warn, format!("sending {name} failed: {e}")),
                };
                self.push(Some(peer), kind, text);
            }
            Event::Notice(n) => self.console(LineKind::System, n),
            Event::Warning(w) => self.console(LineKind::Warn, w),
            Event::Stopped => {}
        }
    }

    /// Executes one parsed command.
    pub fn handle(
        &mut self,
        cmd: Cmd,
        profile: &Profile,
        settings: &mut Settings,
        ctx: &egui::Context,
    ) -> Action {
        match cmd {
            Cmd::Text(text) => self.send_text(text),
            Cmd::Help => {
                let mut s = String::from("COMMANDS\n");
                for (c, d) in commands::HELP {
                    s.push_str(&format!("  {c:<26} {d}\n"));
                }
                self.reply(LineKind::Block, s.trim_end());
            }
            Cmd::Invite { stop: true } => self.engine.send(Command::ClosePairing),
            Cmd::Invite { stop: false } => self.invite(settings, ctx),
            Cmd::Add(None) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("GhostChat Contact Key", &[contact_key::FILE_EXTENSION])
                    .pick_file()
                {
                    self.add_from_file(&path);
                }
            }
            Cmd::Add(Some(text)) => self.add_key_text(&text),
            Cmd::Accept(name) => self.answer(name.as_deref(), true),
            Cmd::Reject(name) => self.answer(name.as_deref(), false),
            Cmd::Verify(name) => {
                if let Some(peer) = self.resolve(name.as_deref()) {
                    return Action::Verify(peer);
                }
            }
            Cmd::Connect(name) => {
                if let Some(peer) = self.resolve(name.as_deref()) {
                    if self.trust(&peer) == Some(Trust::KeyChanged) {
                        let n = self.name(&peer);
                        self.reply(
                            LineKind::Alert,
                            format!("blocked: {n}'s key changed. Run /verify {n} first."),
                        );
                    } else {
                        self.reply(
                            LineKind::System,
                            format!("connecting to {}...", self.name(&peer)),
                        );
                        self.engine.send(Command::Connect(peer));
                    }
                }
            }
            Cmd::Disconnect(name) => {
                if let Some(peer) = self.resolve(name.as_deref()) {
                    self.engine.send(Command::Disconnect(peer));
                }
            }
            Cmd::Chat(None) => self.selected = None,
            Cmd::Chat(Some(name)) => {
                if let Some(peer) = self.resolve(Some(&name)) {
                    self.select(Some(peer));
                }
            }
            Cmd::Remove(name) => {
                if let Some(peer) = self.resolve(name.as_deref()) {
                    let n = self.name(&peer);
                    self.confirm = Some(Confirm::Remove(peer));
                    self.reply(
                        LineKind::Warn,
                        format!("Delete contact {n}? You would need to pair again. /yes or /no"),
                    );
                }
            }
            Cmd::Alias { name, alias } => {
                if let Some(peer) = self.resolve(Some(&name)) {
                    let alias = alias
                        .map(|a| {
                            sanitize::text(&a)
                                .replace('\n', " ")
                                .trim()
                                .chars()
                                .take(ALIAS_MAX)
                                .collect::<String>()
                                .to_uppercase()
                        })
                        .filter(|a| !a.is_empty());
                    self.contacts
                        .lock()
                        .expect("contacts")
                        .set_alias(&peer, alias);
                    self.save_contacts(profile);
                    self.reply(
                        LineKind::Ok,
                        format!("contact now shown as {}", self.name(&peer)),
                    );
                }
            }
            Cmd::Send(path) => self.send_file_cmd(path),
            Cmd::Codename(None) => {
                let me = settings.codename.clone().unwrap_or_default();
                self.reply(
                    LineKind::System,
                    format!("your codename is {me}. Change it with /codename NEWNAME"),
                );
            }
            Cmd::Codename(Some(new)) => match codename::normalize(&new) {
                Ok(name) => {
                    settings.codename = Some(name.clone());
                    self.engine.send(Command::SetCodename(name.clone()));
                    self.reply(
                        LineKind::Ok,
                        format!("you are now {name}. Your contacts will see the change."),
                    );
                }
                Err(e) => self.reply(LineKind::Warn, e.to_string()),
            },
            Cmd::MyKey { save } => self.show_my_key(settings, ctx, save),
            Cmd::Theme(None) => {
                let names: Vec<&str> = ThemeName::ALL.iter().map(|t| t.label()).collect();
                self.reply(
                    LineKind::System,
                    format!(
                        "themes: {} (now: {})",
                        names.join(", "),
                        settings.theme.label()
                    ),
                );
            }
            Cmd::Theme(Some(t)) => match ThemeName::parse(&t) {
                Some(theme) => settings.theme = theme,
                None => self.reply(LineKind::Warn, "themes: green, amber, ice"),
            },
            Cmd::Effects(Some(on)) => {
                settings.scanlines = on;
                settings.flicker = on;
            }
            Cmd::Effects(None) => self.reply(
                LineKind::System,
                format!(
                    "effects: scanlines {}, flicker {} — /fx on or /fx off",
                    on_off(settings.scanlines),
                    on_off(settings.flicker)
                ),
            ),
            Cmd::Contacts => self.list_contacts(),
            Cmd::Clear => {
                self.convos.remove(&self.selected);
            }
            Cmd::Panic => return Action::Panic,
            Cmd::PanicWipe(Some(on)) => {
                settings.panic_wipes_identity = on;
                if on {
                    self.reply(
                        LineKind::Alert,
                        "PANIC WILL NOW ALSO DELETE your identity and contacts (you would need to pair again).",
                    );
                } else {
                    self.reply(
                        LineKind::Ok,
                        "panic wipes memory only; identity and contacts are kept.",
                    );
                }
            }
            Cmd::PanicWipe(None) => self.reply(
                LineKind::System,
                format!(
                    "panic also deletes identity: {} — /panicwipe on|off",
                    on_off(settings.panic_wipes_identity)
                ),
            ),
            Cmd::Quit => return Action::Quit,
            Cmd::Yes => self.confirm(true, profile),
            Cmd::No => self.confirm(false, profile),
            Cmd::Unknown(msg) => self.reply(LineKind::Warn, msg),
        }
        Action::None
    }

    fn send_text(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let Some(peer) = self.selected else {
            self.reply(
                LineKind::Warn,
                "pick a contact on the left first (this is the console)",
            );
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        self.push(
            Some(peer),
            LineKind::Mine {
                id,
                status: Delivery::Sent,
            },
            text.clone(),
        );
        self.engine.send(Command::SendText {
            to: peer,
            id,
            body: text,
        });
    }

    fn send_file_cmd(&mut self, path: Option<String>) {
        let Some(peer) = self.selected else {
            self.reply(
                LineKind::Warn,
                "open a conversation first (click a contact), then /send or drop the file",
            );
            return;
        };
        let path = match path {
            Some(p) => Some(std::path::PathBuf::from(p.trim().trim_matches('"'))),
            None => rfd::FileDialog::new().pick_file(),
        };
        let Some(path) = path else {
            return;
        };
        self.engine.send(Command::SendFile { to: peer, path });
    }

    pub fn select(&mut self, peer: Option<PublicKey>) {
        self.selected = peer;
        if let Some(p) = peer {
            self.unread.remove(&p);
        }
    }

    fn invite(&mut self, settings: &Settings, ctx: &egui::Context) {
        let minutes = u64::from(settings.pairing_minutes.clamp(1, 60));
        let window = Duration::from_secs(minutes * 60);
        self.engine.send(Command::OpenPairing(window));
        self.pairing_until = Some(Instant::now() + window);
        let key = self.my_contact_key(settings).encode();
        ctx.copy_text(key.clone());
        self.reply(
            LineKind::Block,
            format!(
                "YOUR CONTACT KEY (copied to clipboard):\n\n{key}\n\n\
                 1. Send it to your family member (e.g. on Signal or WhatsApp).\n\
                 2. They open GhostChat, type /add and paste it.\n\
                 3. You will see INCOMING PAIR REQUEST here — type /accept.\n\
                 Pairing window open for {minutes} minutes (/invite stop to close)."
            ),
        );
    }

    fn add_from_file(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() <= 1024 => {
                self.add_key_text(&String::from_utf8_lossy(&bytes));
            }
            Ok(_) => self.reply(LineKind::Warn, "that file is too large to be a Contact Key"),
            Err(e) => self.reply(LineKind::Warn, format!("could not read file: {e}")),
        }
    }

    fn add_key_text(&mut self, text: &str) {
        let ck = match ContactKey::parse(text) {
            Ok(ck) => ck,
            Err(e) => {
                self.reply(LineKind::Warn, e.to_string());
                return;
            }
        };
        if ck.key == self.me {
            self.reply(LineKind::Warn, "that is your own Contact Key");
            return;
        }
        let outcome = self
            .contacts
            .lock()
            .expect("contacts")
            .classify(&ck.key, &ck.codename);
        if let AddOutcome::KeyChanged { .. } = outcome {
            self.alert_until = Some(Instant::now() + Duration::from_secs(2));
            self.reply(
                LineKind::Alert,
                format!(
                    "INTRUSION WARNING: KEY MISMATCH — you already have a VERIFIED {0} with a DIFFERENT key.\n\
                     Only continue if {0} told you by voice that they reinstalled GhostChat.\n\
                     Continue? /yes or /no",
                    ck.codename
                ),
            );
            self.confirm = Some(Confirm::AddKeyChange(ck));
            return;
        }
        self.reply(
            LineKind::System,
            format!(
                "Sending pair request to {}. They must have /invite open and press /accept.",
                ck.codename
            ),
        );
        self.engine.send(Command::Pair(ck));
    }

    /// `/accept` and `/reject`: file offers in the open conversation first,
    /// then pair requests.
    fn answer(&mut self, name: Option<&str>, accept: bool) {
        let file_idx = match name {
            None => self
                .file_requests
                .iter()
                .rposition(|r| Some(r.peer) == self.selected),
            Some(n) => self
                .file_requests
                .iter()
                .rposition(|r| r.name.eq_ignore_ascii_case(n.trim())),
        };
        if let Some(i) = file_idx {
            let req = self.file_requests.remove(i);
            self.engine.send(Command::FileDecision {
                peer: req.peer,
                id: req.id,
                accept,
            });
            if accept {
                self.push(
                    Some(req.peer),
                    LineKind::System,
                    format!("receiving {}...", req.name),
                );
                self.transfers.insert(
                    (req.peer, req.id, true),
                    Transfer {
                        name: req.name,
                        total: 0,
                        done: 0,
                    },
                );
            } else {
                self.push(
                    Some(req.peer),
                    LineKind::System,
                    format!("{} refused", req.name),
                );
            }
            return;
        }
        let idx = match name {
            None => self.pair_requests.len().checked_sub(1),
            Some(n) => {
                let n = n.trim().to_uppercase();
                self.pair_requests
                    .iter()
                    .rposition(|r| r.codename == n || self.name(&r.peer).to_uppercase() == n)
            }
        };
        let Some(idx) = idx else {
            self.reply(LineKind::Warn, "nothing to accept or reject");
            return;
        };
        let req = self.pair_requests.remove(idx);
        self.engine.send(Command::PairDecision {
            peer: req.peer,
            accept,
        });
        if !accept {
            self.console(
                LineKind::System,
                format!("pair request from {} rejected", req.codename),
            );
        }
    }

    fn confirm(&mut self, yes: bool, profile: &Profile) {
        let Some(c) = self.confirm.take() else {
            self.reply(LineKind::Warn, "nothing to confirm");
            return;
        };
        if !yes {
            self.reply(LineKind::System, "cancelled");
            return;
        }
        match c {
            Confirm::AddKeyChange(ck) => {
                self.reply(
                    LineKind::System,
                    format!("Sending pair request to {}...", ck.codename),
                );
                self.engine.send(Command::Pair(ck));
            }
            Confirm::Remove(peer) => {
                let name = self.name(&peer);
                self.contacts.lock().expect("contacts").remove(&peer);
                self.save_contacts(profile);
                self.engine.send(Command::ContactsUpdated);
                self.convos.remove(&Some(peer));
                self.presence.remove(&peer);
                if self.selected == Some(peer) {
                    self.selected = None;
                }
                self.console(LineKind::Ok, format!("{name} removed"));
            }
        }
    }

    fn show_my_key(&mut self, settings: &Settings, ctx: &egui::Context, save: bool) {
        let ck = self.my_contact_key(settings);
        let key = ck.encode();
        ctx.copy_text(key.clone());
        self.reply(
            LineKind::Block,
            format!(
                "YOUR CONTACT KEY (public — safe to share; copied to clipboard):\n\n{key}\n\n{}\n\n\
                 Safety words are different for each contact: see /verify NAME.",
                safety::key_art(&self.me)
            ),
        );
        if save
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!(
                    "{}.{}",
                    ck.codename.to_lowercase(),
                    contact_key::FILE_EXTENSION
                ))
                .add_filter("GhostChat Contact Key", &[contact_key::FILE_EXTENSION])
                .save_file()
        {
            match std::fs::write(&path, ck.to_file_contents()) {
                Ok(()) => self.reply(LineKind::Ok, format!("saved {}", path.display())),
                Err(e) => self.reply(LineKind::Warn, format!("could not save: {e}")),
            }
        }
    }

    fn list_contacts(&mut self) {
        let lines: Vec<String> = {
            let book = self.contacts.lock().expect("contacts");
            book.iter()
                .map(|c| {
                    let presence = match self.presence.get(&c.key) {
                        Some(Presence::Online) => "ONLINE",
                        Some(Presence::Connecting) => "CONNECTING",
                        _ => "OFFLINE",
                    };
                    format!(
                        "  {:<22} {:<10} {:<12} codename {}  id {}",
                        book.display_name(&c.key),
                        presence,
                        c.trust.label(),
                        c.codename,
                        c.key.fmt_short()
                    )
                })
                .collect()
        };
        if lines.is_empty() {
            self.reply(LineKind::System, "no contacts yet — /invite or /add");
        } else {
            self.reply(LineKind::Block, format!("CONTACTS\n{}", lines.join("\n")));
        }
    }

    pub fn on_dropped_file(&mut self, path: &Path) {
        let is_key = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(contact_key::FILE_EXTENSION));
        if is_key {
            self.add_from_file(path);
        } else {
            self.send_file_cmd(Some(path.display().to_string()));
        }
    }

    // ------------------------------------------------------------------
    // Rendering

    /// Security status for the selected contact, built only from real engine
    /// and contact state (PRD §12). Peer text never reaches this panel.
    pub fn status_bar(&self, ui: &mut egui::Ui, p: &Palette, settings: &Settings) {
        ui.horizontal(|ui| {
            let me = settings.codename.clone().unwrap_or_default();
            ui.label(
                RichText::new(format!("GHOSTCHAT // {me}"))
                    .color(p.bright)
                    .strong(),
            );
            if let Some(until) = self.pairing_until {
                let left = until.saturating_duration_since(Instant::now()).as_secs();
                ui.label(
                    RichText::new(format!("PAIRING OPEN {}:{:02}", left / 60, left % 60))
                        .color(p.warn),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (net, color) = if self.net_online {
                    ("NET ONLINE", p.fg)
                } else {
                    ("NET OFFLINE", p.warn)
                };
                ui.label(RichText::new(net).color(color));
                ui.label(RichText::new("|").color(p.dim));
                for (text, color) in self.security_status(p) {
                    ui.label(RichText::new(text).color(color).strong());
                }
            });
        });
    }

    /// Right-to-left list of status segments.
    pub fn security_status(&self, p: &Palette) -> Vec<(String, Color32)> {
        let Some(peer) = self.selected else {
            return vec![("CONSOLE".into(), p.dim)];
        };
        let mut out = Vec::new();
        match self.trust(&peer) {
            Some(Trust::Verified) => out.push(("VERIFIED".into(), p.bright)),
            Some(t) => out.push((t.label().into(), p.danger)),
            None => out.push(("UNKNOWN".into(), p.danger)),
        }
        match self.presence.get(&peer) {
            Some(Presence::Online) => {
                let path = match self.paths.get(&peer) {
                    Some(PathKind::Direct) => "DIRECT",
                    Some(PathKind::Relay) => "RELAY",
                    _ => "PATH ?",
                };
                out.push((path.into(), p.fg));
                out.push(("E2EE".into(), p.bright));
            }
            Some(Presence::Connecting) => out.push(("CONNECTING".into(), p.warn)),
            _ => out.push(("OFFLINE".into(), p.dim)),
        }
        out
    }

    /// Progress bars for transfers with the open conversation.
    pub fn transfer_bars(&self, ui: &mut egui::Ui, p: &Palette) {
        for ((peer, _, incoming), t) in &self.transfers {
            if Some(*peer) != self.selected {
                continue;
            }
            let frac = if t.total > 0 {
                (t.done as f64 / t.total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = (frac * 30.0) as usize;
            let bar = format!("{}{}", "#".repeat(filled), ".".repeat(30 - filled));
            let dir = if *incoming { "<<" } else { ">>" };
            ui.label(
                RichText::new(format!(
                    "{dir} [{bar}] {:>3}% {} {}",
                    (frac * 100.0) as u32,
                    t.name,
                    human_size(t.done)
                ))
                .color(p.fg)
                .font(mono(13.0)),
            );
        }
    }

    pub fn has_transfers_here(&self) -> bool {
        self.transfers
            .keys()
            .any(|(p, ..)| Some(*p) == self.selected)
    }

    pub fn prompt_banner(&self, ui: &mut egui::Ui, p: &Palette) {
        for r in &self.pair_requests {
            let (text, color) = if matches!(r.outcome, AddOutcome::KeyChanged { .. }) {
                (
                    format!(
                        "INTRUSION WARNING: KEY MISMATCH — pair request from {} with a NEW key. /accept {0} or /reject {0}",
                        r.codename
                    ),
                    p.danger,
                )
            } else {
                (
                    format!(
                        "INCOMING PAIR REQUEST: {} — /accept {0} or /reject {0}",
                        r.codename
                    ),
                    p.warn,
                )
            };
            ui.label(RichText::new(text).color(color).strong());
        }
        if self.confirm.is_some() {
            ui.label(
                RichText::new("CONFIRMATION NEEDED — type /yes or /no")
                    .color(p.warn)
                    .strong(),
            );
        }
    }

    pub fn contact_list(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.label(RichText::new("CHANNELS").color(p.dim));
        if ui
            .selectable_label(self.selected.is_none(), "# CONSOLE")
            .clicked()
        {
            self.select(None);
        }
        ui.add_space(4.0);
        let entries: Vec<(PublicKey, String, Trust)> = {
            let book = self.contacts.lock().expect("contacts");
            book.iter()
                .map(|c| (c.key, book.display_name(&c.key), c.trust))
                .collect()
        };
        for (key, name, trust) in entries {
            let dot = match self.presence.get(&key) {
                Some(Presence::Online) => "●",
                Some(Presence::Connecting) => "~",
                _ => "○",
            };
            let badge = match trust {
                Trust::Verified => "[V]",
                Trust::Added => "[?]",
                Trust::KeyChanged => "[!]",
            };
            let unread = if self.unread.contains(&key) { " *" } else { "" };
            let color = match trust {
                Trust::Verified => p.fg,
                _ => p.danger,
            };
            let label = RichText::new(format!("{dot} {name} {badge}{unread}")).color(color);
            if ui
                .selectable_label(self.selected == Some(key), label)
                .clicked()
            {
                self.select(Some(key));
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(
                RichText::new(concat!("GhostChat v", env!("CARGO_PKG_VERSION")))
                    .color(p.dim)
                    .small(),
            );
            ui.label(
                RichText::new(
                    "● online  ○ offline  ~ connecting\n[V] verified  [?] unverified\n[!] key changed: blocked\n>  sent   >> delivered   x failed",
                )
                    .color(p.dim)
                    .small(),
            );
        });
    }

    pub fn conversation(&self, ui: &mut egui::Ui, p: &Palette) {
        let peer_name = self.selected.map(|k| self.name(&k)).unwrap_or_default();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                for line in self.convos.get(&self.selected).into_iter().flatten() {
                    // Each line is a plain-text label: no markup in peer text is interpreted.
                    let rt = match &line.kind {
                        LineKind::System => RichText::new(&line.text).color(p.dim),
                        LineKind::Ok => RichText::new(&line.text).color(p.fg),
                        LineKind::Warn => RichText::new(format!("! {}", line.text)).color(p.warn),
                        LineKind::Alert => RichText::new(format!("!! {}", line.text))
                            .color(p.danger)
                            .strong(),
                        LineKind::Block => {
                            RichText::new(&line.text).color(p.bright).font(mono(14.0))
                        }
                        LineKind::Theirs => {
                            RichText::new(format!("{peer_name}> {}", line.text)).color(p.bright)
                        }
                        LineKind::Mine { status, .. } => {
                            let mark = match status {
                                Delivery::Sent => "> ",
                                Delivery::Delivered => ">>",
                                Delivery::Failed(_) => "x ",
                            };
                            RichText::new(format!("{mark} YOU> {}", line.text)).color(p.fg)
                        }
                    };
                    ui.add(egui::Label::new(rt).wrap());
                }
            });
    }

    pub fn is_alert(&self) -> bool {
        self.alert_until.is_some_and(|t| Instant::now() < t)
    }
}

pub fn my_key(me: PublicKey, settings: &Settings) -> ContactKey {
    ContactKey::new(me, settings.codename.clone().unwrap_or_default())
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}
