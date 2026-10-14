//! Headless UI flow: two GhostChat windows on loopback go through first run,
//! remote pairing, safety-word verification and a chat — driven by keyboard
//! events like a user would. Screenshots go to `target/ui-shots/`.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use ghostchat::{
    engine::NetConfig,
    safety,
    storage::{Lookup, Profile, SecretStore},
    ui::App,
};
use iroh::address_lookup::MemoryLookup;

struct Win {
    h: Harness<'static, App>,
    _dir: tempfile::TempDir,
}

fn open(rt: &tokio::runtime::Runtime, name: &str, lookup: &MemoryLookup) -> Win {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::at(dir.path().to_path_buf(), Some(name));
    // Never write test downloads into the real Downloads folder.
    profile
        .save_settings(&ghostchat::storage::Settings {
            downloads_dir: Some(dir.path().join("downloads")),
            ..Default::default()
        })
        .unwrap();
    let handle = rt.handle().clone();
    let net = NetConfig::Local {
        lookup: lookup.clone(),
    };
    let h = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .wgpu()
        .build_eframe(move |cc| {
            App::with_store(
                cc,
                handle,
                profile,
                SecretStore::Memory(Default::default()),
                Lookup::Missing,
                Some(net),
            )
        });
    Win { h, _dir: dir }
}

impl Win {
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.h.step();
        }
    }

    fn type_line(&mut self, text: &str) {
        self.h.event(egui::Event::Text(text.to_string()));
        self.frames(2);
        self.h.key_press(egui::Key::Enter);
        self.frames(3);
    }

    fn app(&self) -> &App {
        self.h.state()
    }

    /// Steps frames until `f` holds (network events arrive asynchronously).
    fn wait(&mut self, what: &str, f: impl Fn(&App) -> bool) {
        let start = Instant::now();
        while !f(self.app()) {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out waiting for {what}; screen={} lines={:#?}",
                self.app().screen_name(),
                self.app().visible_lines()
            );
            self.h.step();
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn shot(&mut self, name: &str) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
        std::fs::create_dir_all(&dir).unwrap();
        match self.h.render() {
            Ok(img) => img.save(dir.join(format!("{name}.png"))).unwrap(),
            // No GPU adapter on some CI machines: the flow is still tested.
            Err(e) => eprintln!("screenshot {name} skipped: {e}"),
        }
    }

    fn has_line(&self, needle: &str) -> bool {
        has(self.app(), needle)
    }
}

fn has(app: &App, needle: &str) -> bool {
    app.visible_lines().iter().any(|l| l.contains(needle))
}

#[test]
fn first_run_pair_verify_chat() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lookup = MemoryLookup::new();
    let mut alice = open(&rt, "ui-alice", &lookup);
    let mut bob = open(&rt, "ui-bob", &lookup);

    // Boot → codename (any key skips the boot animation).
    for w in [&mut alice, &mut bob] {
        w.frames(2);
        w.h.key_press(egui::Key::Space);
        w.frames(3);
        assert_eq!(w.app().screen_name(), "codename");
    }
    alice.shot("01-codename");
    alice.type_line("alice");
    bob.type_line("bob");
    assert_eq!(alice.app().screen_name(), "main");
    assert_eq!(bob.app().screen_name(), "main");
    alice.wait("node online", |a| has(a, "listening"));
    alice.shot("02-main-empty");

    // Invite / add / accept.
    alice.type_line("/invite");
    let key = alice.app().contact_key().unwrap();
    assert!(alice.has_line(&key), "invite shows the Contact Key");
    alice.shot("03-invite");
    bob.type_line(&format!("/add {key}"));
    alice.wait("pair request", |a| has(a, "INCOMING PAIR REQUEST: BOB"));
    alice.shot("04-pair-request");
    alice.type_line("/accept");
    alice.wait("bob added", |a| has(a, "BOB added (UNVERIFIED)"));
    bob.wait("alice added", |b| has(b, "ALICE added (UNVERIFIED)"));

    // Verify on Alice's side with the words Bob reads aloud.
    let (ak, bk) = (
        alice.app().public_key().unwrap(),
        bob.app().public_key().unwrap(),
    );
    let bob_reads = safety::for_pair(&bk, &ak).mine.join(" ");
    alice.type_line("/verify BOB");
    assert_eq!(alice.app().screen_name(), "verify");
    alice.shot("05-verify");
    alice.type_line("wrong words entirely here ok");
    assert_eq!(alice.app().screen_name(), "verify", "wrong words rejected");
    alice.type_line(&bob_reads);
    // Alice's words stay on screen: Bob may not have typed them yet.
    assert_eq!(alice.app().screen_name(), "verify-done");
    alice.shot("05b-verify-done");
    alice.h.key_press(egui::Key::Enter);
    alice.frames(2);
    assert_eq!(alice.app().screen_name(), "main");
    // Already verified: /verify only shows Alice's words again, nothing to type.
    alice.type_line("/verify BOB");
    assert_eq!(alice.app().screen_name(), "verify-done");
    alice.h.key_press(egui::Key::Escape);
    alice.frames(2);
    assert_eq!(alice.app().screen_name(), "main");

    // Chat both ways.
    alice.type_line("/chat BOB");
    alice.wait("bob verified", |a| has(a, "BOB is now VERIFIED"));
    alice.wait("secure channel", |a| has(a, "SECURE CHANNEL ESTABLISHED"));
    alice.frames(3);
    alice.shot("10-handshake");
    alice.type_line("hello bob 你好");
    bob.type_line("/chat ALICE");
    bob.wait("message", |b| has(b, "ALICE> hello bob 你好"));
    // Everything so far happened in a focused window: no alert.
    assert_eq!(alice.app().attention_requests(), 0);
    // Window in the background: a new message flashes the taskbar.
    alice.h.input_mut().focused = false;
    bob.type_line("hi alice");
    alice.wait("reply", |a| has(a, "BOB> hi alice"));
    alice.frames(2);
    assert_eq!(
        alice.app().attention_requests(),
        1,
        "background message alerts"
    );
    alice.h.input_mut().focused = true;

    // Voice call through the UI (silent audio in tests).
    alice.type_line("/call BOB");
    bob.wait("ringing", |b| has(b, "INCOMING CALL from ALICE"));
    bob.frames(2);
    bob.shot("13-call-ringing");
    bob.type_line("/answer");
    alice.wait("call connected", |a| has(a, "CALL CONNECTED with BOB"));
    alice.frames(3);
    alice.shot("14-call-active");
    alice.type_line("/hangup");
    bob.wait("call ended", |b| has(b, "CALL ENDED with ALICE"));
    alice.frames(5);
    alice.shot("06-chat");
    bob.shot("07-chat-bob-unverified");

    // File transfer through the UI: /send, receiver sees the offer, /accept.
    let file = alice._dir.path().join("holiday.txt");
    std::fs::write(&file, "beach 2026").unwrap();
    alice.type_line(&format!("/send {}", file.display()));
    bob.wait("file offer", |b| {
        has(b, "INCOMING FILE from ALICE: holiday.txt")
    });
    bob.shot("09-file-offer");
    bob.type_line("/accept");
    bob.wait("file saved", |b| has(b, "Received holiday.txt"));
    alice.wait("file delivered", |a| has(a, "holiday.txt delivered"));
    let saved = bob._dir.path().join("downloads").join("holiday.txt");
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "beach 2026");

    // /clear wipes the conversation from memory.
    alice.type_line("/clear");
    assert!(!alice.has_line("hello bob"));

    for theme in ["amber", "ice", "green"] {
        alice.type_line(&format!("/theme {theme}"));
        alice.frames(2);
        alice.shot(&format!("12-theme-{theme}"));
    }

    // Idle screen hides the conversation; any key wakes it.
    alice.h.state_mut().simulate_idle();
    alice.frames(3);
    alice.shot("11-idle");
    alice.h.key_press(egui::Key::Space);
    alice.frames(2);

    // F12 = panic: session state gone, peer sees the channel close.
    alice.type_line("another secret");
    alice.h.key_press(egui::Key::F12);
    alice.frames(2);
    assert_eq!(alice.app().screen_name(), "panic");
    assert!(alice.app().visible_lines().is_empty());
    assert!(alice.app().public_key().is_none(), "session state dropped");
    alice.shot("08-panic");
    bob.type_line("/chat");
    bob.wait("channel closed", |b| has(b, "secure channel closed"));
}

/// Regression: the input re-requested focus every frame, and each
/// `request_focus` interrupts the IME, so Cangjie/Pinyin never committed.
#[test]
fn focused_input_does_not_interrupt_ime() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lookup = MemoryLookup::new();
    let mut w = open(&rt, "ui-ime", &lookup);
    w.frames(2);
    w.h.key_press(egui::Key::Space);
    w.frames(3);
    w.type_line("dave");
    assert_eq!(w.app().screen_name(), "main");
    w.frames(3);
    let ime = w.h.output().platform_output.ime.as_ref();
    assert!(ime.is_some(), "focused input must enable the IME");
    assert!(
        !ime.unwrap().should_interrupt_composition,
        "steady frames must not interrupt an IME composition"
    );

    // A composition across several frames commits into the input
    // (`/chat NAME` echoes the unknown name back, so the text is observable).
    w.h.event(egui::Event::Text("/chat ".into()));
    w.frames(1);
    w.h.event(egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "日月".into(),
        active_range_chars: None,
    }));
    w.frames(2);
    w.h.event(egui::Event::Ime(egui::ImeEvent::Commit("明".into())));
    w.frames(2);
    w.h.key_press(egui::Key::Enter);
    w.frames(3);
    assert!(
        w.app()
            .visible_lines()
            .iter()
            .any(|l| l.contains("no single contact called 明")),
        "committed IME text reaches the console: {:#?}",
        w.app().visible_lines()
    );
}

#[test]
fn panic_can_also_wipe_identity() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lookup = MemoryLookup::new();
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::at(dir.path().to_path_buf(), Some("ui-wipe"));
    let mem: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>> = Default::default();
    let store = SecretStore::Memory(mem.clone());
    let handle = rt.handle().clone();
    let net = NetConfig::Local { lookup };
    let p2 = profile.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_eframe(move |cc| App::with_store(cc, handle, p2, store, Lookup::Missing, Some(net)));
    h.step();
    h.key_press(egui::Key::Space);
    h.step();
    for line in ["carol", "/panicwipe on"] {
        h.event(egui::Event::Text(line.into()));
        h.step();
        h.key_press(egui::Key::Enter);
        h.step();
        h.step();
    }
    assert!(mem.lock().unwrap().is_some(), "identity was created");
    assert_eq!(profile.load_settings().codename.as_deref(), Some("CAROL"));
    assert!(profile.load_settings().panic_wipes_identity);
    h.key_press(egui::Key::F12);
    h.step();
    assert_eq!(h.state().screen_name(), "panic");
    assert!(mem.lock().unwrap().is_none(), "identity deleted on panic");
    assert!(!profile.contacts_path().exists());
    assert_eq!(profile.load_settings().codename, None);
}

fn boot_with(
    rt: &tokio::runtime::Runtime,
    profile: Profile,
    store: SecretStore,
    lookup: Lookup,
) -> Harness<'static, App> {
    let handle = rt.handle().clone();
    let net = NetConfig::Local {
        lookup: MemoryLookup::new(),
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_eframe(move |cc| App::with_store(cc, handle, profile, store, lookup, Some(net)));
    h.step();
    h.key_press(egui::Key::Space);
    h.step();
    h.step();
    h
}

fn enter(h: &mut Harness<'static, App>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
}

#[test]
fn unreachable_os_store_never_replaces_an_existing_identity() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::at(dir.path().to_path_buf(), Some("ui-locked"));
    profile
        .save_settings(&ghostchat::storage::Settings {
            codename: Some("GHOST-01".into()),
            ..Default::default()
        })
        .unwrap();
    let store = SecretStore::Passphrase {
        path: profile.vault_path(),
    };
    let h = boot_with(
        &rt,
        profile.clone(),
        store,
        Lookup::OsUnavailable("keyring locked".into()),
    );
    assert_eq!(h.state().screen_name(), "error");
    assert!(
        !profile.vault_path().exists(),
        "no new identity was created"
    );
}

#[test]
fn passphrase_vault_first_run_and_unlock() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::at(dir.path().to_path_buf(), Some("ui-vault"));
    let store = SecretStore::Passphrase {
        path: profile.vault_path(),
    };
    // First run without an OS store: codename, then passphrase twice.
    let mut h = boot_with(
        &rt,
        profile.clone(),
        store.clone(),
        Lookup::OsUnavailable("no secret service".into()),
    );
    assert_eq!(h.state().screen_name(), "codename");
    enter(&mut h, "dave");
    assert_eq!(h.state().screen_name(), "unlock");
    enter(&mut h, "short");
    assert_eq!(h.state().screen_name(), "unlock", "too short is refused");
    enter(&mut h, "correct horse battery");
    enter(&mut h, "correct horse battery");
    assert_eq!(h.state().screen_name(), "main");
    let key = h.state().public_key().unwrap();
    assert!(profile.vault_path().exists());
    drop(h);

    // Next start: wrong passphrase refused, right one restores the same key.
    let mut h = boot_with(&rt, profile.clone(), store, Lookup::NeedsPassphrase);
    assert_eq!(h.state().screen_name(), "unlock");
    enter(&mut h, "wrong passphrase");
    assert_eq!(h.state().screen_name(), "unlock");
    enter(&mut h, "correct horse battery");
    assert_eq!(h.state().screen_name(), "main");
    assert_eq!(
        h.state().public_key(),
        Some(key),
        "same identity after unlock"
    );
}

/// Full real stack except the OS window: OS secure store + n0 relay/discovery.
/// Run manually: `cargo test --test ui real_stack -- --ignored --nocapture`
#[test]
#[ignore = "uses the real OS secure store and the internet"]
fn real_stack_os_store_and_n0() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut wins = Vec::new();
    for name in ["qa-real-a", "qa-real-b"] {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(dir.path().to_path_buf(), Some(name));
        profile
            .save_settings(&ghostchat::storage::Settings {
                downloads_dir: Some(dir.path().join("downloads")),
                ..Default::default()
            })
            .unwrap();
        let (store, lookup) = SecretStore::locate(&profile);
        assert!(
            matches!(store, SecretStore::Os { .. }),
            "OS store available"
        );
        let handle = rt.handle().clone();
        let p2 = profile.clone();
        let h = Harness::builder()
            .with_size(egui::vec2(1100.0, 720.0))
            .build_eframe(move |cc| App::with_store(cc, handle, p2, store, lookup, None));
        wins.push((Win { h, _dir: dir }, profile));
    }
    let (mut a, pa) = wins.remove(0);
    let (mut b, pb) = wins.remove(0);
    for w in [&mut a, &mut b] {
        w.frames(2);
        w.h.key_press(egui::Key::Space);
        w.frames(3);
    }
    a.type_line("qa-real-a");
    b.type_line("qa-real-b");
    a.wait("relay", |x| has(x, "Connected to relay network"));
    b.wait("relay", |x| has(x, "Connected to relay network"));
    a.type_line("/invite");
    let key = a.app().contact_key().unwrap();
    b.type_line(&format!("/add {key}"));
    a.wait("pair request", |x| {
        has(x, "INCOMING PAIR REQUEST: QA-REAL-B")
    });
    a.type_line("/accept");
    b.wait("paired", |x| has(x, "QA-REAL-A added"));
    a.type_line("/chat QA-REAL-B");
    a.wait("channel", |x| has(x, "SECURE CHANNEL ESTABLISHED"));
    a.type_line("over n0 with OS keystore");
    b.type_line("/chat QA-REAL-A");
    b.wait("message", |x| has(x, "QA-REAL-A> over n0 with OS keystore"));
    a.frames(3);
    a.shot("20-real-stack");
    drop(a);
    drop(b);
    // Remove the test identities from the OS secure store.
    for p in [pa, pb] {
        ghostchat::storage::wipe_identity(&p).unwrap();
    }
}

/// Plain `/invite`: two devices both called BOB ask to pair. `/accept BOB` is
/// refused as ambiguous; `/accept CODE` picks the real one.
#[test]
fn duplicate_pair_requests_need_the_invite_code() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lookup = MemoryLookup::new();
    let mut alice = open(&rt, "ui-dup-alice", &lookup);
    let mut bob = open(&rt, "ui-dup-bob", &lookup);
    let mut fake = open(&rt, "ui-dup-fake", &lookup);
    for w in [&mut alice, &mut bob, &mut fake] {
        w.frames(2);
        w.h.key_press(egui::Key::Space);
        w.frames(3);
    }
    alice.type_line("alice");
    bob.type_line("bob");
    fake.type_line("bob");
    alice.wait("node online", |a| has(a, "listening"));

    alice.type_line("/invite");
    let key = alice.app().contact_key().unwrap();
    bob.type_line(&format!("/add {key}"));
    fake.type_line(&format!("/add {key}"));
    let bob_code = safety::invite_code(&bob.app().public_key().unwrap());
    assert!(
        bob.has_line(&format!("Your invite code is {bob_code}")),
        "the requester sees its own code"
    );
    alice.wait("two requests", |a| {
        a.visible_lines()
            .iter()
            .filter(|l| l.contains("INCOMING PAIR REQUEST: BOB"))
            .count()
            == 2
    });
    alice.frames(2);
    alice.shot("15-duplicate-requests");

    alice.type_line("/accept BOB");
    assert!(alice.has_line("2 requests claim to be BOB"));
    alice.type_line(&format!("/accept {}", bob_code.to_lowercase()));
    bob.wait("paired", |b| has(b, "ALICE added"));
    let alice_key = alice.app().public_key().unwrap();
    assert!(
        !fake.has_line("ALICE added"),
        "{alice_key} must not pair the fake"
    );
}

/// `/voicetest` records, plays back and reports. Headless tests have no
/// sound card, so it reports a silent microphone.
#[test]
fn voice_test_runs_and_reports() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let lookup = MemoryLookup::new();
    let mut w = open(&rt, "ui-voicetest", &lookup);
    w.frames(2);
    w.h.key_press(egui::Key::Space);
    w.frames(3);
    w.type_line("tester");
    w.type_line("/voicetest");
    assert!(w.has_line("VOICE TEST: say something"));
    w.frames(3);
    w.shot("16-voice-test");
    w.wait("voice test result", |a| {
        has(a, "VOICE TEST: your microphone picked up (almost) nothing")
    });
}
