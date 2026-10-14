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
    effects,
    theme::{Palette, ThemeName, mono},
};
use crate::{
    codename,
    contact_key::{self, ContactKey},
    contacts::{AddOutcome, ContactBook, Trust},
    engine::{CallState, Command, Delivery, EngineHandle, Event, PathKind, Presence},
    safety, sanitize,
    storage::{self, Profile, Settings},
    transfer::human_size,
    voice,
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
    /// Something arrived that the user should notice (message, pair request,
    /// file offer). The app decides whether to flash the taskbar and clears it.
    pub needs_attention: bool,
    /// The current voice call: contact, state, and when that state began.
    pub call: Option<(PublicKey, CallState, Instant)>,
    /// Microphone muted for calls (kept across calls).
    pub muted: bool,
    /// Live speech levels during a call.
    pub meter: voice::Meter,
    /// Last `/devices` listing (microphones, speakers), so `/mic 2` works.
    pub devices: Option<(Vec<String>, Vec<String>)>,
    /// A running `/voicetest`.
    pub voice_test: Option<voice::VoiceTest>,
    /// No sound hardware (headless UI tests).
    pub silent_audio: bool,
}

impl Core {
    pub fn new(
        engine: EngineHandle,
        events: mpsc::UnboundedReceiver<Event>,
        contacts: Arc<Mutex<ContactBook>>,
        me: PublicKey,
        storage_key: Zeroizing<[u8; 32]>,
        meter: voice::Meter,
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
            needs_attention: false,
            call: None,
            muted: false,
            meter,
            devices: None,
            voice_test: None,
            silent_audio: false,
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

    fn start_voice_test(&mut self, settings: &Settings) {
        if self.call.is_some() {
            self.reply(LineKind::Warn, "not during a call — /hangup first");
            return;
        }
        if self.voice_test.is_some() {
            self.reply(LineKind::Warn, "a voice test is already running");
            return;
        }
        let backend = if self.silent_audio {
            voice::AudioBackend::Silent
        } else {
            voice::AudioBackend::Devices(voice::DeviceChoice {
                input: settings.mic.clone(),
                output: settings.speaker.clone(),
            })
        };
        self.voice_test = Some(voice::VoiceTest::start(&backend, self.meter.clone()));
        self.reply(
            LineKind::System,
            format!(
                "VOICE TEST: say something for {} seconds (e.g. count to five). It is then played back so you hear yourself as others will. Nothing leaves this computer.",
                voice::TEST_RECORD.as_secs()
            ),
        );
    }

    /// Reports a finished `/voicetest`. Called every frame.
    pub fn poll_voice_test(&mut self) {
        let Some(t) = &self.voice_test else {
            return;
        };
        let (kind, text) = match t.phase() {
            voice::TestPhase::Done if t.peak() < VOICE_TEST_QUIET => (
                LineKind::Warn,
                "VOICE TEST: your microphone picked up (almost) nothing. Check that it is not muted or turned down in the Windows sound settings, or choose another with /devices."
                    .to_string(),
            ),
            voice::TestPhase::Done => (
                LineKind::Ok,
                "[ OK ] VOICE TEST: microphone works. Did you hear yourself played back?\n  \
                 Yes: your speakers work too, and that is how you sound in a call.\n  \
                 No: check the speakers or headphones, or choose others with /devices."
                    .to_string(),
            ),
            voice::TestPhase::Failed => (
                LineKind::Warn,
                format!(
                    "VOICE TEST failed: {}",
                    sanitize::text(&t.error().unwrap_or_default())
                ),
            ),
            _ => return,
        };
        self.voice_test = None;
        self.reply(kind, text);
    }

    /// The bottom bar while `/voicetest` runs.
    pub fn voice_test_panel(&self, ui: &mut egui::Ui, p: &Palette) -> Option<Cmd> {
        let t = self.voice_test.as_ref()?;
        let mut clicked = None;
        ui.horizontal(|ui| {
            let (title, meter) = match t.phase() {
                voice::TestPhase::Recording => (
                    "<< VOICE TEST >> RECORDING — say something",
                    format!("MIC {}", level_bar(self.meter.mic())),
                ),
                _ => (
                    "<< VOICE TEST >> PLAYING BACK — this is how you sound to others",
                    format!("OUT {}", level_bar(self.meter.peer())),
                ),
            };
            ui.label(RichText::new(title).color(p.bright).strong());
            ui.label(RichText::new(meter).color(p.fg));
            if ui
                .button(RichText::new("[ STOP ]").color(p.danger).strong())
                .clicked()
            {
                clicked = Some(Cmd::VoiceTest { stop: true });
            }
        });
        if t.phase() == voice::TestPhase::Recording && self.meter.no_signal() {
            ui.label(
                RichText::new(
                    "Your microphone sends no sound — check it is not muted or turned down in the Windows sound settings, or choose another with /devices.",
                )
                .color(p.danger),
            );
        }
        clicked
    }

    fn list_devices(&mut self, settings: &Settings) {
        let (mics, speakers) = match voice::device::list() {
            Ok(d) => d,
            Err(e) => {
                self.reply(LineKind::Warn, format!("cannot list audio devices: {e}"));
                return;
            }
        };
        let section = |title: &str, names: &[String], chosen: &Option<String>| {
            let mut out = format!("{title}:\n");
            let default = if chosen.is_none() { "  <- in use" } else { "" };
            out.push_str(&format!("  0) system default{default}\n"));
            for (i, n) in names.iter().enumerate() {
                let mark = if chosen.as_deref() == Some(n.as_str()) {
                    "  <- in use"
                } else {
                    ""
                };
                out.push_str(&format!("  {}) {}{mark}\n", i + 1, sanitize::text(n)));
            }
            out
        };
        let text = format!(
            "{}\n{}\nChoose with /mic N or /speaker N (0 = system default). Applies to the next call.",
            section("MICROPHONES", &mics, &settings.mic),
            section("SPEAKERS", &speakers, &settings.speaker),
        );
        self.devices = Some((mics, speakers));
        self.reply(LineKind::Block, text);
    }

    /// `/mic N` or `/speaker N`: N from the last `/devices` list, 0 or
    /// "default" for the system default.
    fn choose_device(&mut self, settings: &mut Settings, input: bool, arg: Option<String>) {
        let what = if input { "microphone" } else { "speakers" };
        let Some(arg) = arg else {
            let current = if input {
                &settings.mic
            } else {
                &settings.speaker
            };
            let name = current
                .as_deref()
                .map_or("system default".into(), sanitize::text);
            self.reply(
                LineKind::System,
                format!(
                    "{what}: {name} — /devices to list, /{} N to choose",
                    if input { "mic" } else { "speaker" }
                ),
            );
            return;
        };
        let choice = if arg.eq_ignore_ascii_case("default") || arg == "0" {
            None
        } else {
            if self.devices.is_none() {
                self.devices = voice::device::list().ok();
            }
            let names = self
                .devices
                .as_ref()
                .map(|(mics, speakers)| if input { mics } else { speakers });
            let picked = arg
                .parse::<usize>()
                .ok()
                .and_then(|n| names.and_then(|list| list.get(n.checked_sub(1)?)));
            match picked {
                Some(name) => Some(name.clone()),
                None => {
                    self.reply(
                        LineKind::Warn,
                        format!("no {what} number {} — see /devices", sanitize::text(&arg)),
                    );
                    return;
                }
            }
        };
        let label = choice
            .as_deref()
            .map_or("system default".into(), sanitize::text);
        if input {
            settings.mic = choice;
        } else {
            settings.speaker = choice;
        }
        self.engine
            .send(Command::SetAudioDevices(voice::DeviceChoice {
                input: settings.mic.clone(),
                output: settings.speaker.clone(),
            }));
        self.reply(
            LineKind::Ok,
            format!("{what} set to {label} (used from the next call)"),
        );
    }

    /// A call line goes to the contact's conversation and, if the user is
    /// looking somewhere else, there too.
    fn call_line(&mut self, peer: PublicKey, kind: LineKind, text: String) {
        if self.selected != Some(peer) {
            let here = self.selected;
            self.push(here, kind.clone(), text.clone());
        }
        self.push(Some(peer), kind, text);
    }

    fn on_call(&mut self, peer: PublicKey, state: CallState) {
        let name = self.name(&peer);
        let (kind, text) = match &state {
            CallState::Calling => (
                LineKind::System,
                format!("CALLING {name}... (/hangup to cancel)"),
            ),
            CallState::Ringing => (
                LineKind::Alert,
                format!("INCOMING CALL from {name}  —  /answer or /hangup"),
            ),
            CallState::Active => (
                LineKind::Ok,
                format!("[ OK ] CALL CONNECTED with {name} · E2EE · /mute · /hangup"),
            ),
            CallState::Ended(reason) => (
                LineKind::System,
                format!("CALL ENDED with {name} ({})", sanitize::text(reason)),
            ),
        };
        if state == CallState::Ringing {
            // Short red glitch, the same effect as an intrusion alert.
            self.alert_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        // The devices belong to the call now.
        self.voice_test = None;
        self.call = match state {
            CallState::Ended(_) => None,
            s => Some((peer, s, Instant::now())),
        };
        self.call_line(peer, kind, text);
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
            self.needs_attention |= ev.wants_attention();
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
                let code = safety::invite_code(&peer);
                let mut msg = format!(
                    "INCOMING PAIR REQUEST: {codename} [{code}]  —  ask {codename} for their invite code; if it is {code}: /accept {code}, otherwise /reject {code}"
                );
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
            Event::Call { peer, state } => self.on_call(peer, state),
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
            Cmd::Help(topic) => match commands::help_text(topic.as_deref()) {
                Ok(text) => self.reply(LineKind::Block, text),
                Err(e) => self.reply(LineKind::Warn, e),
            },
            Cmd::Invite { stop: true, .. } => self.engine.send(Command::ClosePairing),
            Cmd::Invite { stop: false, code } => self.invite(settings, ctx, code),
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
            Cmd::Flash(Some(on)) => settings.flash = on,
            Cmd::Flash(None) => self.reply(
                LineKind::System,
                format!(
                    "taskbar flash for messages in the background: {} — /flash on|off",
                    on_off(settings.flash)
                ),
            ),
            Cmd::Call(name) => {
                if let Some(peer) = self.resolve(name.as_deref()) {
                    self.engine.send(Command::Call(peer));
                }
            }
            Cmd::Answer => match &self.call {
                Some((peer, CallState::Ringing, _)) => self.engine.send(Command::AnswerCall {
                    peer: *peer,
                    accept: true,
                }),
                _ => self.reply(LineKind::Warn, "no incoming call to answer"),
            },
            Cmd::Hangup => match &self.call {
                Some((peer, CallState::Ringing, _)) => self.engine.send(Command::AnswerCall {
                    peer: *peer,
                    accept: false,
                }),
                Some((peer, _, _)) => self.engine.send(Command::Hangup(*peer)),
                None => self.reply(LineKind::Warn, "no call to end"),
            },
            Cmd::Mute(on) => {
                self.muted = on.unwrap_or(!self.muted);
                self.engine.send(Command::MuteCall(self.muted));
                self.reply(
                    LineKind::System,
                    if self.muted {
                        "microphone MUTED for calls — /mute to unmute"
                    } else {
                        "microphone on for calls"
                    },
                );
            }
            Cmd::Devices => self.list_devices(settings),
            Cmd::VoiceTest { stop: true } => {
                if self.voice_test.take().is_some() {
                    self.reply(LineKind::System, "voice test stopped");
                }
            }
            Cmd::VoiceTest { stop: false } => self.start_voice_test(settings),
            Cmd::Mic(arg) => self.choose_device(settings, true, arg),
            Cmd::Speaker(arg) => self.choose_device(settings, false, arg),
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

    fn invite(&mut self, settings: &Settings, ctx: &egui::Context, code: Option<String>) {
        let code = match code {
            None => None,
            Some(raw) => match safety::parse_invite_code(&raw) {
                None => {
                    self.reply(
                        LineKind::Warn,
                        "an invite code has 12 letters/digits, like 7F3A-91C2-XK4P",
                    );
                    return;
                }
                Some(c) if c == safety::invite_code(&self.me) => {
                    self.reply(
                        LineKind::Warn,
                        "that is your own invite code — ask the other person for theirs",
                    );
                    return;
                }
                Some(c) => Some(c),
            },
        };
        let minutes = u64::from(settings.pairing_minutes.clamp(1, 60));
        let window = Duration::from_secs(minutes * 60);
        self.engine.send(Command::OpenPairing {
            duration: window,
            code: code.clone(),
        });
        self.pairing_until = Some(Instant::now() + window);
        let key = self.my_contact_key(settings).encode();
        ctx.copy_text(key.clone());
        let step3 = match &code {
            Some(c) => format!(
                "3. Only the device with invite code {c} is accepted, automatically.\n   \
                 Requests from any other device are rejected."
            ),
            None => "3. You will see INCOMING PAIR REQUEST here with their invite code.\n   \
                     Check that code with them by voice, then /accept CODE."
                .to_string(),
        };
        self.reply(
            LineKind::Block,
            format!(
                "YOUR CONTACT KEY (copied to clipboard):\n\n{key}\n\n\
                 1. Send it to your family member (e.g. on Signal or WhatsApp).\n\
                 2. They open GhostChat, type /add and paste it.\n\
                 {step3}\n\
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
                "PAIR REQUEST SENT to {}. Your invite code is {} — tell it to them\n\
                 (they use /invite {1}, or check it before /accept).",
                ck.codename,
                safety::invite_code(&self.me)
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
            None if self.pair_requests.len() > 1 => {
                self.reply(
                    LineKind::Warn,
                    "several pair requests are waiting — name the invite code: /accept CODE",
                );
                return;
            }
            None => self.pair_requests.len().checked_sub(1),
            Some(n) => {
                // The invite code identifies one key; a codename may not.
                let by_code = safety::parse_invite_code(n).and_then(|c| {
                    self.pair_requests
                        .iter()
                        .position(|r| safety::invite_code(&r.peer) == c)
                });
                let n = n.trim().to_uppercase();
                let by_name: Vec<usize> = self
                    .pair_requests
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.codename == n || self.name(&r.peer).to_uppercase() == n)
                    .map(|(i, _)| i)
                    .collect();
                if by_code.is_none() && by_name.len() > 1 {
                    self.reply(
                        LineKind::Alert,
                        format!(
                            "{} requests claim to be {n} — use the invite code: /accept CODE",
                            by_name.len()
                        ),
                    );
                    return;
                }
                by_code.or(by_name.first().copied())
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
                "YOUR INVITE CODE: {}  (someone who invites you types /invite {0})\n\n\
                 YOUR CONTACT KEY (public — safe to share; copied to clipboard):\n\n{key}\n\n{}\n\n\
                 Safety words are different for each contact: see /verify NAME.",
                safety::invite_code(&self.me),
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
    /// Shown above the input while calling, ringing or in a call. Returns
    /// the command of a clicked button.
    pub fn call_panel(&self, ui: &mut egui::Ui, p: &Palette) -> Option<Cmd> {
        let (peer, state, since) = self.call.as_ref()?;
        let name = self.name(peer);
        let t = ui.input(|i| i.time);
        let blink = (t * 2.0) as i64 % 2 == 0;
        let mut clicked = None;
        let mut button = |ui: &mut egui::Ui, label: &str, color: Color32, cmd: Cmd| {
            if ui
                .button(RichText::new(label).color(color).strong())
                .clicked()
            {
                clicked = Some(cmd);
            }
        };
        match state {
            CallState::Ringing => {
                ui.horizontal(|ui| {
                    let color = if blink { p.danger } else { p.bright };
                    ui.label(
                        RichText::new(format!(">>> INCOMING SECURE CALL: {name} <<<"))
                            .color(color)
                            .strong()
                            .size(18.0),
                    );
                    button(ui, "[ ANSWER ]", p.bright, Cmd::Answer);
                    button(ui, "[ DECLINE ]", p.danger, Cmd::Hangup);
                });
            }
            CallState::Calling => {
                ui.horizontal(|ui| {
                    let dots = ".".repeat((t * 2.0) as usize % 4);
                    ui.label(
                        RichText::new(format!("CALLING {name}{dots}"))
                            .color(p.warn)
                            .strong(),
                    );
                    button(ui, "[ CANCEL ]", p.danger, Cmd::Hangup);
                });
            }
            _ => {
                let secs = since.elapsed().as_secs();
                let path = match self.paths.get(peer) {
                    Some(PathKind::Direct) => "DIRECT",
                    Some(PathKind::Relay) => "RELAY",
                    _ => "...",
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "<< SECURE VOICE CHANNEL >> {name}  {}:{:02}  E2EE · {path}",
                            secs / 60,
                            secs % 60
                        ))
                        .color(p.bright)
                        .strong(),
                    );
                    let (label, cmd) = if self.muted {
                        ("[ UNMUTE ]", Cmd::Mute(Some(false)))
                    } else {
                        ("[ MUTE ]", Cmd::Mute(Some(true)))
                    };
                    button(ui, label, p.warn, cmd);
                    button(ui, "[ HANG UP ]", p.danger, Cmd::Hangup);
                });
                ui.horizontal(|ui| {
                    let no_signal = !self.muted && self.meter.no_signal();
                    let (mic, color) = if self.muted {
                        ("MIC [ MUTED          ]".to_string(), p.fg)
                    } else if no_signal {
                        ("MIC [ NO SIGNAL      ]".to_string(), p.danger)
                    } else {
                        (format!("MIC {}", level_bar(self.meter.mic())), p.fg)
                    };
                    ui.label(RichText::new(mic).color(color));
                    if !self.muted && self.meter.echo_guard() {
                        ui.label(RichText::new("ECHO GUARD").color(p.warn).small());
                    }
                    ui.label(
                        RichText::new(format!("IN {}", level_bar(self.meter.peer()))).color(p.fg),
                    );
                    // Decoration only: random bytes, never real keys or audio.
                    ui.label(
                        RichText::new(format!("ENC> {}", effects::random_hex(12)))
                            .color(p.dim)
                            .small(),
                    );
                });
                if !self.muted && self.meter.no_signal() {
                    ui.label(
                        RichText::new(
                            "Your microphone sends no sound — check it is not muted or turned down in the Windows sound settings, or choose another with /devices.",
                        )
                        .color(p.danger),
                    );
                }
            }
        }
        clicked
    }

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
            if let Some((peer, state, since)) = &self.call {
                let name = self.name(peer);
                let (text, color) = match state {
                    CallState::Calling => (format!("CALLING {name}..."), p.warn),
                    CallState::Ringing => (format!("INCOMING CALL: {name}"), p.danger),
                    _ => {
                        let t = since.elapsed().as_secs();
                        let mute = if self.muted { " · MUTED" } else { "" };
                        (
                            format!("CALL {name} {}:{:02}{mute}", t / 60, t % 60),
                            p.bright,
                        )
                    }
                };
                ui.label(RichText::new(text).color(color).strong());
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

    /// Codenames used by more than one waiting pair request.
    fn duplicate_request_names(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut dup = Vec::new();
        for r in &self.pair_requests {
            if !seen.insert(r.codename.as_str()) && !dup.contains(&r.codename) {
                dup.push(r.codename.clone());
            }
        }
        dup
    }

    pub fn prompt_banner(&self, ui: &mut egui::Ui, p: &Palette) {
        for name in self.duplicate_request_names() {
            ui.label(
                RichText::new(format!(
                    "!! {} DEVICES CLAIM TO BE {name} — someone else has your Contact Key. \
                     Accept only the code the real {name} reads to you, or /reject all and /invite again.",
                    self.pair_requests
                        .iter()
                        .filter(|r| r.codename == name)
                        .count()
                ))
                .color(p.danger)
                .strong(),
            );
        }
        for r in &self.pair_requests {
            let code = safety::invite_code(&r.peer);
            let (text, color) = if matches!(r.outcome, AddOutcome::KeyChanged { .. }) {
                (
                    format!(
                        "INTRUSION WARNING: KEY MISMATCH — pair request from {} [{code}] with a NEW key. /accept {code} or /reject {code}",
                        r.codename
                    ),
                    p.danger,
                )
            } else {
                (
                    format!(
                        "INCOMING PAIR REQUEST: {} [{code}] — check the code with them, then /accept {code} or /reject {code}",
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
            // Always visible, so nobody has to look for it when being invited.
            ui.label(
                RichText::new(safety::invite_code(&self.me))
                    .color(p.bright)
                    .strong(),
            );
            ui.label(RichText::new("INVITE CODE").color(p.dim).small());
            ui.add_space(6.0);
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

/// Below this peak level a voice test heard (almost) nothing.
const VOICE_TEST_QUIET: f32 = 0.1;

/// `[||||||..........]`: ASCII so it renders with the bundled font.
fn level_bar(level: f32) -> String {
    const CELLS: usize = 16;
    let filled = ((level.clamp(0.0, 1.0) * CELLS as f32).round() as usize).min(CELLS);
    format!("[{}{}]", "|".repeat(filled), ".".repeat(CELLS - filled))
}
