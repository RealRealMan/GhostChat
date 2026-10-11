//! The GhostChat window: a terminal-looking egui app (ARCHITECTURE §3).
//!
//! Screens: boot → (unlock | codename) → main ⇄ verify.
//! Conversation text lives only in memory and is zeroized when dropped.

pub mod commands;
pub mod core;
pub mod effects;
pub mod flash;
pub mod theme;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText};
use iroh::PublicKey;
use tokio::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

use self::core::{Action, Core, LineKind};
use crate::{
    codename,
    contacts::ContactBook,
    engine::{self, Command, EngineConfig, NetConfig},
    safety::{self, Safety},
    sanitize,
    storage::{self, Lookup, Profile, SecretStore, Secrets, Settings},
    voice,
};
use theme::{Palette, mono};

const BOOT_TIME: Duration = Duration::from_millis(2400);
const BOOT_TEXT: &str = concat!(
    "GHOSTCHAT v",
    env!("CARGO_PKG_VERSION"),
    " :: Your Private Channel\n\
[ OK ] Loading identity...\n\
[ OK ] Checking key store...\n\
[ OK ] Starting QUIC/TLS 1.3 transport...\n\
[ OK ] Opening secure channel..."
);
const AUTO_CONNECT_EVERY: Duration = Duration::from_secs(30);
const MIN_PASSPHRASE: usize = 8;

enum Screen {
    Boot,
    Codename {
        input: String,
        error: Option<String>,
    },
    Unlock {
        create: bool,
        input: Zeroizing<String>,
        first: Option<Zeroizing<String>>,
        error: Option<String>,
    },
    Main,
    Verify {
        peer: PublicKey,
        safety: Safety,
        input: String,
        error: Option<String>,
        /// This side is verified: keep showing our words (the peer may still
        /// be typing them) instead of asking for theirs.
        done: bool,
    },
    Panic {
        started: Instant,
    },
    /// Cannot continue safely (e.g. the OS key store holding an existing
    /// identity is unreachable). Shown instead of silently making a new key.
    Error(String),
}

const PANIC_ANIMATION: Duration = Duration::from_millis(700);

pub struct App {
    rt: tokio::runtime::Handle,
    profile: Profile,
    settings: Settings,
    store: SecretStore,
    lookup: Option<Lookup>,
    secrets: Option<Secrets>,
    screen: Screen,
    core: Option<Core>,
    started: Instant,
    boot_notes: Vec<String>,
    ctx: egui::Context,
    last_input: Instant,
    last_frame: Instant,
    rain: effects::Rain,
    /// Replaces the internet configuration (UI tests run on loopback).
    net_override: Option<NetConfig>,
    /// How often the taskbar was flashed for the user; read by tests.
    attention_requests: u32,
    taskbar: flash::TaskbarFlash,
    /// Mirrors `settings.flash` for the engine callback (another thread).
    flash_on: Arc<AtomicBool>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        rt: tokio::runtime::Handle,
        profile: Profile,
    ) -> Self {
        let (store, lookup) = SecretStore::locate(&profile);
        Self::with_store(cc, rt, profile, store, lookup, None)
    }

    /// Like [`Self::new`] with an explicit secret store and network; used by
    /// the headless UI tests.
    pub fn with_store(
        cc: &eframe::CreationContext<'_>,
        rt: tokio::runtime::Handle,
        profile: Profile,
        store: SecretStore,
        lookup: Lookup,
        net_override: Option<NetConfig>,
    ) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let settings = profile.load_settings();
        theme::apply(&cc.egui_ctx, settings.theme);
        let mut boot_notes = Vec::new();
        if let Lookup::OsUnavailable(e) = &lookup {
            boot_notes.push(format!(
                "[WARN] OS secure store unavailable ({}); using a passphrase vault",
                sanitize::text(e)
            ));
        }
        App {
            rt,
            profile,
            flash_on: Arc::new(AtomicBool::new(settings.flash)),
            settings,
            store,
            lookup: Some(lookup),
            secrets: None,
            screen: Screen::Boot,
            core: None,
            started: Instant::now(),
            boot_notes,
            ctx: cc.egui_ctx.clone(),
            last_input: Instant::now(),
            last_frame: Instant::now(),
            rain: effects::Rain::default(),
            net_override,
            attention_requests: 0,
            taskbar: flash::TaskbarFlash::from_window(cc),
        }
    }

    /// Short name of the current screen (for tests and diagnostics).
    pub fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Boot => "boot",
            Screen::Codename { .. } => "codename",
            Screen::Unlock { .. } => "unlock",
            Screen::Main => "main",
            Screen::Verify { done: false, .. } => "verify",
            Screen::Verify { done: true, .. } => "verify-done",
            Screen::Panic { .. } => "panic",
            Screen::Error(_) => "error",
        }
    }

    /// Pretends the user has been away (tests and screenshots of the idle screen).
    #[doc(hidden)]
    pub fn simulate_idle(&mut self) {
        self.last_input = Instant::now() - Duration::from_secs(24 * 3600);
    }

    /// This user's public Contact Key, once the identity is loaded.
    pub fn contact_key(&self) -> Option<String> {
        let core = self.core.as_ref()?;
        Some(core::my_key(core.me, &self.settings).encode())
    }

    /// Public key of this identity, once loaded.
    pub fn public_key(&self) -> Option<PublicKey> {
        self.core.as_ref().map(|c| c.me)
    }

    /// Text of every line in the visible conversation (for tests).
    pub fn visible_lines(&self) -> Vec<String> {
        self.core
            .as_ref()
            .map(|c| c.visible_lines())
            .unwrap_or_default()
    }

    fn save_settings(&mut self) -> Result<(), String> {
        self.profile
            .save_settings(&self.settings)
            .map_err(|e| e.to_string())
    }

    /// Leaves the boot screen once the animation has run.
    fn finish_boot(&mut self) {
        match self.lookup.take() {
            Some(Lookup::Found(secrets)) => {
                self.secrets = Some(secrets);
                self.after_unlock();
            }
            Some(Lookup::Missing) => self.ask_codename(),
            Some(Lookup::OsUnavailable(e)) => {
                // An existing identity lives in the OS store: a new passphrase
                // vault would silently replace it and every contact would see
                // a key change. Only a genuine first run may fall back.
                let existing =
                    self.settings.codename.is_some() || self.profile.contacts_path().exists();
                if existing {
                    self.screen = Screen::Error(format!(
                        "GhostChat cannot open the {} where your identity is stored.\n\n\
                         Details: {}\n\n\
                         Log out and in again (or unlock your keyring), then restart GhostChat.\n\
                         Starting over instead would need a new pairing with every contact:\n\
                         run  ghostchat --wipe-identity  and start GhostChat again.",
                        SecretStore::Os {
                            service: String::new()
                        }
                        .describe(),
                        sanitize::text(&e)
                    ));
                } else {
                    self.ask_codename();
                }
            }
            Some(Lookup::NeedsPassphrase) => {
                self.screen = Screen::Unlock {
                    create: false,
                    input: Zeroizing::new(String::new()),
                    first: None,
                    error: None,
                }
            }
            None => {}
        }
    }

    fn ask_codename(&mut self) {
        self.started = Instant::now();
        self.screen = Screen::Codename {
            input: String::new(),
            error: None,
        };
    }

    fn after_unlock(&mut self) {
        match self.settings.codename.clone() {
            Some(name) if codename::validate(&name).is_ok() => self.start_core(name),
            _ => self.ask_codename(),
        }
    }

    fn submit_codename(&mut self, input: &str) -> Result<(), String> {
        let name = codename::normalize(input).map_err(|e| e.to_string())?;
        self.settings.codename = Some(name.clone());
        self.save_settings()?;
        if self.secrets.is_some() {
            self.start_core(name);
            return Ok(());
        }
        // First run: create the identity.
        if let SecretStore::Passphrase { .. } = self.store {
            self.screen = Screen::Unlock {
                create: true,
                input: Zeroizing::new(String::new()),
                first: None,
                error: None,
            };
            return Ok(());
        }
        let secrets = Secrets::generate();
        self.store
            .save(&secrets, None)
            .map_err(|e| format!("could not store identity: {e}"))?;
        self.secrets = Some(secrets);
        self.start_core(name);
        Ok(())
    }

    fn submit_passphrase(&mut self) {
        let Screen::Unlock {
            create,
            input,
            first,
            error,
        } = &mut self.screen
        else {
            return;
        };
        let typed = std::mem::replace(input, Zeroizing::new(String::new()));
        if !*create {
            match self.store.unlock(&typed) {
                Ok(s) => {
                    self.secrets = Some(s);
                    self.after_unlock();
                }
                Err(e) => *error = Some(e.to_string()),
            }
            return;
        }
        if typed.chars().count() < MIN_PASSPHRASE {
            *error = Some(format!("use at least {MIN_PASSPHRASE} characters"));
            return;
        }
        match first.take() {
            None => {
                *first = Some(typed);
                *error = None;
            }
            Some(f) if *f == *typed => {
                let secrets = Secrets::generate();
                match self.store.save(&secrets, Some(&typed)) {
                    Ok(()) => {
                        self.secrets = Some(secrets);
                        self.after_unlock();
                    }
                    Err(e) => *error = Some(e.to_string()),
                }
            }
            Some(_) => *error = Some("the two passphrases differ — start again".into()),
        }
    }

    fn start_core(&mut self, codename: String) {
        let Some(secrets) = self.secrets.take() else {
            return;
        };
        let mut notes = std::mem::take(&mut self.boot_notes);
        let path = self.profile.contacts_path();
        let book = match storage::load_contacts(&path, &secrets.storage_key) {
            Ok(b) => b,
            Err(e) => {
                // Keep the damaged file instead of overwriting it on the next save.
                let _ = std::fs::rename(&path, path.with_extension("bin.damaged"));
                notes.push(format!(
                    "[WARN] contacts could not be loaded ({e}); starting with an empty list"
                ));
                ContactBook::default()
            }
        };
        let contacts = Arc::new(Mutex::new(book));
        let net = self.net_config(&mut notes);
        let (tx, events) = mpsc::unbounded_channel();
        let ctx = self.ctx.clone();
        let taskbar = self.taskbar.clone();
        let flash_on = self.flash_on.clone();
        let meter = voice::Meter::default();
        let me = secrets.secret_key().public();
        let engine = engine::spawn(
            &self.rt,
            EngineConfig {
                secret_key: secrets.secret_key(),
                codename: codename.clone(),
                contacts: contacts.clone(),
                net,
                auto_connect: self.settings.auto_connect.then_some(AUTO_CONNECT_EVERY),
                downloads: self.profile.downloads_dir(&self.settings),
                max_file_size: self.settings.max_file_mb.saturating_mul(1024 * 1024),
                // UI tests (loopback network) run without sound hardware.
                audio: if self.net_override.is_some() {
                    voice::AudioBackend::Silent
                } else {
                    voice::AudioBackend::Devices(voice::DeviceChoice {
                        input: self.settings.mic.clone(),
                        output: self.settings.speaker.clone(),
                    })
                },
                meter: meter.clone(),
            },
            tx,
            Arc::new(move |attention| {
                // Runs on the network thread, so it works while the window is
                // minimized and the UI loop is paused.
                if attention && flash_on.load(Ordering::Relaxed) {
                    taskbar.flash_if_background();
                }
                ctx.request_repaint();
            }),
        );
        let mut core = Core::new(
            engine,
            events,
            contacts,
            me,
            Zeroizing::new(secrets.storage_key),
            meter,
        );
        drop(secrets);
        for n in notes {
            core.console(LineKind::Warn, n);
        }
        core.console(
            LineKind::Ok,
            format!(
                "[ OK ] Identity loaded from {} — you are {codename}",
                self.store.describe()
            ),
        );
        core.console(LineKind::Ok, "[ OK ] Opening secure channel...");
        if core.contacts.lock().expect("contacts").is_empty() {
            core.console(
                LineKind::System,
                "No contacts yet.\n  To invite a family member: type /invite and send them your Contact Key.\n  If someone sent YOU a Contact Key: type /add and paste it.",
            );
        }
        core.console(LineKind::System, "Type /help for all commands.");
        self.core = Some(core);
        self.screen = Screen::Main;
    }

    fn net_config(&self, notes: &mut Vec<String>) -> NetConfig {
        if let Some(net) = &self.net_override {
            return net.clone();
        }
        let relay_urls = self
            .settings
            .relay_urls
            .iter()
            .filter_map(|u| match u.parse() {
                Ok(url) => Some(url),
                Err(_) => {
                    notes.push(format!(
                        "[WARN] ignoring invalid relay URL {}",
                        sanitize::text(u)
                    ));
                    None
                }
            })
            .collect();
        let pkarr_relay = self
            .settings
            .pkarr_relay
            .as_ref()
            .and_then(|u| match u.parse() {
                Ok(url) => Some(url),
                Err(_) => {
                    notes.push(format!(
                        "[WARN] ignoring invalid discovery URL {}",
                        sanitize::text(u)
                    ));
                    None
                }
            });
        NetConfig::Internet {
            relay_urls,
            pkarr_relay,
            relay_only: self.settings.relay_only,
        }
    }

    fn start_verify(&mut self, peer: PublicKey) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let done = core.trust(&peer) == Some(crate::contacts::Trust::Verified);
        self.screen = Screen::Verify {
            peer,
            safety: safety::for_pair(&core.me, &peer),
            input: String::new(),
            error: None,
            done,
        };
    }

    fn submit_verify(&mut self) {
        let Screen::Verify {
            peer,
            safety,
            input,
            error,
            done,
        } = &mut self.screen
        else {
            return;
        };
        if *done {
            self.screen = Screen::Main;
            return;
        }
        let peer = *peer;
        if !safety::check(&safety.theirs, input) {
            *error = Some(
                "WORDS DO NOT MATCH. Ask them to read again slowly. If they still differ, do NOT trust this contact: /remove it and pair again.".into(),
            );
            input.clear();
            return;
        }
        let profile = self.profile.clone();
        if let Some(core) = self.core.as_mut() {
            let ok = core
                .contacts
                .lock()
                .expect("contacts")
                .verify(&peer)
                .is_ok();
            if ok {
                core.save_contacts(&profile);
                core.engine.send(Command::ContactsUpdated);
                let name = core.name(&peer);
                core.push(
                    Some(peer),
                    LineKind::Ok,
                    format!(
                        "[ OK ] {name} is now VERIFIED. If {name} still needs your words: /verify {name}"
                    ),
                );
                core.engine.send(Command::Connect(peer));
            }
            *done = ok;
        }
        if !*done {
            self.screen = Screen::Main;
        }
    }

    /// PRD §11 panic: close connections, wipe memory (and optionally the
    /// identity), show a short glitch, exit.
    fn panic(&mut self) {
        if let Some(mut core) = self.core.take() {
            core.engine.send(Command::Shutdown { panic: true });
            core.wipe_memory();
        }
        self.secrets = None;
        self.lookup = None;
        if self.settings.panic_wipes_identity {
            // Best effort: the app exits either way.
            let _ = self.store.delete();
            let _ = storage::wipe_identity(&self.profile);
        }
        self.screen = Screen::Panic {
            started: Instant::now(),
        };
    }

    fn ui_panic(&mut self, ui: &mut egui::Ui, p: &Palette, started: Instant) {
        let t = started.elapsed();
        let progress = (t.as_secs_f32() / PANIC_ANIMATION.as_secs_f32()).min(1.0);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                effects::glitch(ui.painter(), rect, p, 1.0 - progress * 0.5);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("PANIC — WIPING SESSION {:>3}%", (progress * 100.0) as u32),
                    mono(26.0),
                    p.danger,
                );
            });
        if t >= PANIC_ANIMATION {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ui.ctx().request_repaint();
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        if let Some(core) = self.core.as_mut() {
            core.engine.send(Command::Shutdown { panic: false });
            core.wipe_memory();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ------------------------------------------------------------------
    // Screens

    /// Time without input before the Matrix idle screen, if it is enabled
    /// and applies to the current screen.
    fn idle_limit(&self) -> Option<Duration> {
        (self.settings.idle_minutes > 0 && matches!(self.screen, Screen::Main))
            .then(|| Duration::from_secs(u64::from(self.settings.idle_minutes) * 60))
    }

    /// Flash the taskbar for something the user has not seen, from the UI
    /// loop: covers the idle screen and non-Windows platforms. On Windows a
    /// minimized window is handled by the engine callback (`flash.rs`),
    /// because this loop does not run then. No system notification is shown,
    /// so nothing is written to the OS notification history.
    fn request_attention(&mut self, ctx: &egui::Context) {
        if !self.settings.flash {
            return;
        }
        self.attention_requests += 1;
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }

    /// Number of taskbar flashes so far (for tests).
    pub fn attention_requests(&self) -> u32 {
        self.attention_requests
    }

    /// Matrix rain after `idle_minutes` without input. Also hides the
    /// conversation from someone walking past.
    fn idle_screen(&mut self, ctx: &egui::Context, p: &Palette, dt: f32) {
        let Some(limit) = self.idle_limit() else {
            return;
        };
        let idle_for = self.last_input.elapsed();
        if idle_for < limit {
            ctx.request_repaint_after(limit - idle_for);
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("idle"),
        ));
        let rect = ctx.content_rect();
        self.rain.draw(&painter, rect, p, dt);
        let label_at = rect.center_bottom() - egui::vec2(0.0, 30.0);
        painter.rect_filled(
            egui::Rect::from_center_size(label_at, egui::vec2(280.0, 30.0)),
            4.0,
            p.bg,
        );
        painter.text(
            label_at,
            egui::Align2::CENTER_CENTER,
            "IDLE — press any key",
            mono(16.0),
            p.bright,
        );
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    /// Retro effects and the key-mismatch alert, drawn above everything.
    fn overlays(&self, ctx: &egui::Context, p: &Palette) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("fx"),
        ));
        let rect = ctx.content_rect();
        if self.core.as_ref().is_some_and(|c| c.is_alert()) {
            effects::glitch(&painter, rect, p, 0.6);
        }
        if self.settings.scanlines {
            effects::scanlines(&painter, rect);
        }
        if self.settings.flicker {
            effects::flicker(&painter, rect, ctx.input(|i| i.time));
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui_boot(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let t = self.started.elapsed();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24.0))
            .show(ui, |ui| {
                let typed = effects::typewriter(BOOT_TEXT, t, 90.0);
                ui.label(RichText::new(format!("{typed}_")).color(p.fg));
                for n in &self.boot_notes {
                    ui.label(RichText::new(n).color(p.warn));
                }
                ui.add_space(16.0);
                // Decoration: random bytes only.
                for _ in 0..6 {
                    ui.label(
                        RichText::new(effects::random_hex(20))
                            .color(p.dim)
                            .font(mono(12.0)),
                    );
                }
            });
        let skip = ui.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        });
        if t >= BOOT_TIME || skip {
            self.finish_boot();
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui_codename(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let mut submit = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24.0))
            .show(ui, |ui| {
                let Screen::Codename { input, error } = &mut self.screen else {
                    return;
                };
                let title = effects::typewriter("ENTER YOUR CODENAME", self.started.elapsed(), 18.0);
                ui.label(RichText::new(format!("{title}_")).color(p.bright).size(22.0));
                if title.len() < "ENTER YOUR CODENAME".len() {
                    ui.ctx().request_repaint_after(Duration::from_millis(40));
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "3-16 characters: letters A-Z, digits, '-' and '_'. Example: NIGHTOWL\nYour family sees this name. You can change it later with /codename.",
                    )
                    .color(p.dim),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(">").color(p.fg).size(20.0));
                    let r = ui.add(
                        egui::TextEdit::singleline(input)
                            .font(mono(20.0))
                            .desired_width(320.0)
                            .char_limit(codename::MAX_LEN),
                    );
                    keep_focus(&r);
                    if submitted(ui, &r) {
                        submit = Some(input.clone());
                    }
                });
                if let Some(e) = error {
                    ui.label(RichText::new(format!("! {e}")).color(p.danger));
                }
            });
        if let Some(name) = submit
            && let Err(e) = self.submit_codename(&name)
            && let Screen::Codename { error, .. } = &mut self.screen
        {
            *error = Some(e);
        }
    }

    fn ui_unlock(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let mut submit = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24.0))
            .show(ui, |ui| {
                let Screen::Unlock {
                    create,
                    input,
                    first,
                    error,
                } = &mut self.screen
                else {
                    return;
                };
                let (title, hint) = match (*create, first.is_some()) {
                    (false, _) => (
                        "ENTER PASSPHRASE",
                        "Your identity is protected by a passphrase on this computer.",
                    ),
                    (true, false) => (
                        "CHOOSE A PASSPHRASE",
                        "This computer has no OS key store, so your identity key is protected by a passphrase.\nIf you forget it you must pair with your family again.",
                    ),
                    (true, true) => ("REPEAT PASSPHRASE", "Type the same passphrase again."),
                };
                ui.label(RichText::new(title).color(p.bright).size(22.0));
                ui.label(RichText::new(hint).color(p.dim));
                ui.add_space(12.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut **input)
                        .password(true)
                        .font(mono(18.0))
                        .desired_width(360.0),
                );
                keep_focus(&r);
                if submitted(ui, &r) {
                    submit = true;
                }
                if let Some(e) = error {
                    ui.label(RichText::new(format!("! {e}")).color(p.danger));
                }
            });
        if submit {
            self.submit_passphrase();
        }
    }

    fn ui_verify(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let mut submit = false;
        let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let name = match (&self.screen, &self.core) {
            (Screen::Verify { peer, .. }, Some(core)) => core.name(peer),
            _ => return,
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24.0))
            .show(ui, |ui| {
                let Screen::Verify {
                    safety,
                    input,
                    error,
                    done,
                    ..
                } = &mut self.screen
                else {
                    return;
                };
                ui.label(
                    RichText::new(format!("VERIFY {name}"))
                        .color(p.bright)
                        .size(22.0),
                );
                if *done {
                    ui.label(
                        RichText::new(format!(
                            "[ OK ] {name} is VERIFIED on your side.\n\
                             {name} must also verify you: keep reading YOUR words below until {name}'s screen says VERIFIED.\n\
                             You can show this screen again any time with /verify {name}."
                        ))
                        .color(p.fg),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Same picture on both screens:").color(p.fg));
                    ui.label(
                        RichText::new(&safety.randomart)
                            .color(p.bright)
                            .font(mono(16.0)),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("READ THESE WORDS ALOUD to {name}:")).color(p.fg),
                    );
                    ui.label(
                        RichText::new(safety.mine.join("  "))
                            .color(p.bright)
                            .size(20.0),
                    );
                    ui.add_space(10.0);
                    if ui.button("[ DONE (Enter / Esc) ]").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        submit = true;
                    }
                    return;
                }
                ui.label(
                    RichText::new(format!(
                        "1. Call {name} by VOICE or VIDEO and make sure it is really them.\n   Do not do this step by text message."
                    ))
                    .color(p.fg),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("2. Both screens must show this same picture:").color(p.fg));
                ui.label(
                    RichText::new(&safety.randomart)
                        .color(p.bright)
                        .font(mono(16.0)),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("3. READ THESE WORDS ALOUD to {name}:")).color(p.fg),
                );
                ui.label(
                    RichText::new(safety.mine.join("  "))
                        .color(p.bright)
                        .size(20.0),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "4. TYPE the {} words {name} reads to you:",
                        safety::WORDS_PER_SIDE
                    ))
                    .color(p.fg),
                );
                let r = ui.add(
                    egui::TextEdit::singleline(input)
                        .font(mono(18.0))
                        .desired_width(560.0),
                );
                keep_focus(&r);
                if submitted(ui, &r) {
                    submit = true;
                }
                if let Some(e) = error {
                    ui.label(RichText::new(format!("! {e}")).color(p.danger));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("[ CONFIRM ]").clicked() {
                        submit = true;
                    }
                    if ui.button("[ CANCEL (Esc) ]").clicked() {
                        cancel = true;
                    }
                });
            });
        if submit {
            self.submit_verify();
        } else if cancel {
            self.screen = Screen::Main;
        }
    }

    fn ui_main(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let mut action = Action::None;
        let settings_before = self.settings.clone();
        let profile = self.profile.clone();
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let alert = core.is_alert();

        egui::Panel::top("status")
            .frame(egui::Frame::new().fill(p.panel).inner_margin(6.0))
            .show(ui, |ui| core.status_bar(ui, p, &self.settings));

        if !core.pair_requests.is_empty() || core.confirm.is_some() {
            let fill = if alert {
                p.danger.gamma_multiply(0.3)
            } else {
                p.panel
            };
            egui::Panel::top("prompt")
                .frame(egui::Frame::new().fill(fill).inner_margin(6.0))
                .show(ui, |ui| core.prompt_banner(ui, p));
        }

        egui::Panel::bottom("input")
            .frame(egui::Frame::new().fill(p.panel).inner_margin(6.0))
            .show(ui, |ui| {
                if core.has_transfers_here() {
                    core.transfer_bars(ui, p);
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
                ui.horizontal(|ui| {
                    let me = self.settings.codename.clone().unwrap_or_default();
                    ui.label(RichText::new(format!("{me}@ghostchat:~$")).color(p.dim));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut core.input)
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .font(mono(theme::FONT_SIZE)),
                    );
                    if submitted(ui, &r) {
                        let mut text = std::mem::take(&mut core.input);
                        action =
                            core.handle(commands::parse(&text), &profile, &mut self.settings, &ctx);
                        text.zeroize();
                    }
                    keep_focus(&r);
                });
                let hint = match core.selected {
                    Some(k) => format!(
                        "message to {} · Enter = send · /help · F12 = panic",
                        core.name(&k)
                    ),
                    None => "CONSOLE · type /help for commands · F12 = panic".to_string(),
                };
                ui.label(RichText::new(hint).color(p.dim).small());
            });

        if core.call.is_some() {
            // Plain background: the ringing glitch overlay is the effect, and
            // the buttons must stay readable.
            let clicked = egui::Panel::bottom("call")
                .frame(egui::Frame::new().fill(p.panel).inner_margin(6.0))
                .show(ui, |ui| core.call_panel(ui, p))
                .inner;
            if let Some(cmd) = clicked {
                action = core.handle(cmd, &profile, &mut self.settings, &ctx);
            }
            // Level meters and the call timer move while a call is up.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::left("contacts")
            .frame(egui::Frame::new().fill(p.panel).inner_margin(6.0))
            .default_size(220.0)
            .show(ui, |ui| core.contact_list(ui, p));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(8.0))
            .show(ui, |ui| {
                core.conversation(ui, p);
                core.handshakes
                    .retain(|(_, at)| at.elapsed() < effects::HANDSHAKE);
                if let Some(peer) = core.selected
                    && let Some((_, at)) = core.handshakes.iter().find(|(k, _)| *k == peer)
                {
                    let path = match core.paths.get(&peer) {
                        Some(engine::PathKind::Direct) => "DIRECT (peer-to-peer)",
                        Some(engine::PathKind::Relay) => {
                            "RELAY (encrypted, relay sees ciphertext only)"
                        }
                        _ => "negotiating",
                    };
                    let hex = data_encoding::HEXLOWER.encode(&peer.as_bytes()[..12]);
                    effects::handshake(
                        ui.painter(),
                        ui.max_rect(),
                        p,
                        at.elapsed(),
                        &core.name(&peer),
                        &format!("{hex}..."),
                        path,
                    );
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
            });

        // Files dropped on the window: .ghostkey = add contact, others = send.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        for path in dropped {
            core.on_dropped_file(&path);
        }

        if self.settings != settings_before {
            self.flash_on.store(self.settings.flash, Ordering::Relaxed);
            if self.settings.theme != settings_before.theme {
                theme::apply(&ctx, self.settings.theme);
            }
            if let Err(e) = profile.save_settings(&self.settings) {
                core.console(LineKind::Warn, format!("could not save settings: {e}"));
            }
        }
        if alert {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        if core.pairing_until.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        match action {
            Action::None => {}
            Action::Verify(peer) => self.start_verify(peer),
            Action::Panic => self.panic(),
            Action::Quit => self.quit(&ctx),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let profile = self.profile.clone();
        if let Some(core) = self.core.as_mut() {
            core.drain_events(&profile);
        }
        let wants_attention = self
            .core
            .as_mut()
            .is_some_and(|c| std::mem::take(&mut c.needs_attention));
        if wants_attention {
            let in_background = !ui.input(|i| i.focused);
            let idle = self
                .idle_limit()
                .is_some_and(|limit| self.last_input.elapsed() >= limit);
            if in_background || idle {
                self.request_attention(ui.ctx());
            }
        }
        let p = self.settings.theme.palette();
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let active = ui.input(|i| {
            !i.events.is_empty()
                || i.pointer.delta() != egui::Vec2::ZERO
                || !i.raw.dropped_files.is_empty()
        });
        if active {
            self.last_input = now;
        }
        if ui.input(|i| i.key_pressed(egui::Key::F12))
            && !matches!(self.screen, Screen::Panic { .. })
        {
            self.panic();
        }
        let before = std::mem::discriminant(&self.screen);
        match self.screen {
            Screen::Boot => self.ui_boot(ui, &p),
            Screen::Codename { .. } => self.ui_codename(ui, &p),
            Screen::Unlock { .. } => self.ui_unlock(ui, &p),
            Screen::Main => self.ui_main(ui, &p),
            Screen::Verify { .. } => self.ui_verify(ui, &p),
            Screen::Panic { started } => self.ui_panic(ui, &p, started),
            Screen::Error(ref msg) => {
                let msg = msg.clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(p.bg).inner_margin(24.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("CANNOT START").color(p.danger).size(22.0));
                        ui.add_space(8.0);
                        ui.label(RichText::new(msg).color(p.fg));
                    });
            }
        }
        if !matches!(self.screen, Screen::Panic { .. }) {
            self.overlays(ui.ctx(), &p);
            self.idle_screen(ui.ctx(), &p, dt);
        }
        // A screen change happens mid-frame; draw the new screen right away.
        if std::mem::discriminant(&self.screen) != before {
            ui.ctx().request_repaint();
        }
    }

    fn on_exit(&mut self) {
        if let Some(core) = self.core.as_mut() {
            core.engine.send(Command::Shutdown { panic: false });
            core.wipe_memory();
        }
    }
}

/// Keeps keyboard focus on the screen's only input. Re-requests focus only
/// when it was lost: every `request_focus` call also interrupts the IME, so
/// calling it each frame cancelled Chinese (e.g. Cangjie) compositions before
/// they could commit.
fn keep_focus(r: &egui::Response) {
    if !r.has_focus() {
        r.request_focus();
    }
}

/// Enter pressed in this input. Checked directly (not only via `lost_focus`)
/// because the input re-grabs focus after Enter.
fn submitted(ui: &egui::Ui, r: &egui::Response) -> bool {
    (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter))
}
