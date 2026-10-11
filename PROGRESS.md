# GhostChat — Progress

Status as of 2026-10-11, release **v0.1.1**. Source of truth: PRD-v0.3.md.

The development history was squashed into a single commit for the public
repository, so per-phase commits no longer exist.

| Phase | Status |
|---|---|
| 0 Architecture | Done — `ARCHITECTURE.md`, `OPEN-QUESTIONS.md` |
| 1 Walking skeleton | Done |
| 2 Identity & contacts | Done |
| 3 Chat polish / panic / no-traces | Done |
| 4 Packaging | Done — all installers build in CI; only Windows installed on real machines |
| 5 File transfer | Done |
| 6 Hacker UX | Done |
| 7 Hardening | Done (fuzz smoke runs in CI) |

## Verified

### GitHub Actions (v0.1.0)

- `ci.yml` green on Windows, macOS and Linux: `cargo fmt --check`,
  `cargo clippy --all-targets -D warnings`, `cargo test`.
- `cargo audit` job green.
- Fuzz job green: 60-second `cargo fuzz` smoke run of each target
  (`contact_key`, `chat_frame`, `pair_frame`).
- `release.yml` green for all four packages: Windows `.exe` (NSIS),
  macOS `.dmg` (Apple Silicon and Intel), Linux `.AppImage` + `.deb`.
  Downloadable from the run's Artifacts with SHA-256 checksums.

### Local (Windows 11)

- `cargo test`: 93 tests pass (55 unit, 6 chat, 5 pairing, 7 files,
  6 hostile-peer, 9 property-fuzz, 5 headless UI).
- Ignored tests, run manually:
  - `tests/internet.rs`: two engines over **n0's public relay + DNS discovery**:
    chat works; relay-only mode reports `RELAY` correctly.
  - `ui.rs::real_stack_os_store_and_n0`: two full apps (headless) with the
    **real Windows Credential Manager** and n0: first run, invite/add/accept,
    chat. Test credentials were deleted afterwards.
  - `storage::os_secure_store_roundtrip`: Credential Manager read/write/delete.
- Headless UI tests render every screen to `target/ui-shots/*.png`; reviewed
  visually.
- Windows installer (NSIS, per-user): install → Start-menu + desktop shortcut,
  Add/Remove Programs entry → app launches → `--wipe-identity` works →
  silent uninstall removes files, shortcuts and registry entry.

### Manual test by the owner (two Windows PCs)

- The two PCs were on **different networks** (one on a phone hotspot), so the
  connection went across the internet through NAT, not over a local network.
- Install, codename, `/invite` → `/add` → `/accept`, safety-word verification,
  chat, file transfer, panic: all working.
- Cangjie (Chinese IME) input works after the IME fix below.
- 0.1.1: the taskbar flashes when a message arrives while GhostChat is
  minimized, and stops when the window is opened.

## Not verified / known gaps

- **macOS and Linux installs**: packages build in CI but have not been
  installed or run on a real Mac or Linux machine (Gatekeeper flow, Keychain
  prompt, AppImage/`.deb` install, Secret Service fallback, Kali).
- **Uninstaller "also delete identity" tick box**: only the silent (unticked)
  path was tested automatically.
- **Non-technical tester install** (PRD §17 manual test): not done.
- Optional app PIN (PRD §13) not implemented (OPEN-QUESTIONS #8).
- Memory wiping is best effort (egui text caches, OS IME/clipboard) — see
  SECURITY.md §2.

## Bugs found and fixed

During the autonomous build:

- Boot screen never left without mouse movement (missing repaint on screen
  change).
- Enter did not submit any input field; caught by the headless UI test.
- Re-opening `/invite` while already open sent no confirmation event.
- If the OS key store was temporarily unreachable, a new identity would have
  been created silently (looks like a key change to all contacts) — now an
  error screen.
- `✓`/`✗` glyphs missing from the bundled font — replaced with ASCII markers.

After manual testing:

- **Chinese IME input did nothing**: inputs re-requested focus every frame,
  and each `request_focus` interrupts the IME composition. Focus is now only
  re-requested when lost (regression test `focused_input_does_not_interrupt_ime`).
- **Safety words disappeared too early**: after one side verified, its words
  vanished before the other side had typed them. The verify screen now stays
  open until closed, and `/verify NAME` on a verified contact shows your words
  again.
