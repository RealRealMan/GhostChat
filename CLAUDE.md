# CLAUDE.md — GhostChat

Project context for Claude Code sessions on any machine. Read `PROGRESS.md`
first for the current status, then `ARCHITECTURE.md` for design decisions.

## What this is

Private, end-to-end encrypted, live 1-to-1 chat for a family, with a
movie-style hacker terminal look. Rust, egui/eframe window (not a real
terminal), iroh 1.3 (QUIC + TLS 1.3) for transport. Spec: `PRD-v0.3.md`.

## Rules that must not be broken

- No custom cryptography. Transport security comes from iroh/rustls.
- No message content on disk, in logs or in OS notification history.
  This is why alerts use a taskbar flash, not system pop-ups.
- Never put personal names or private email addresses in code, docs, tests,
  examples or commits. Use neutral roles (inviter / contact, 邀請方 / 通話對象)
  and codenames such as `NIGHTOWL`. Commits use the GitHub noreply address.
- Never display secret keys, even in UI effects.
- All peer-supplied text is sanitized before display.
- `main` holds the released version. New work goes on a branch.

## Current state

- `main`: v0.1.1 (tag `v0.1.1`; previous release `v0.1.0`). Windows tested
  on real machines, including across two different networks. macOS/Linux
  packages build in CI but have never been installed on a real machine.
- New in 0.1.1 (developed on branch `feature/background-alerts`):
  - Taskbar flash when a message, pair request or file offer arrives while
    GhostChat is in the background (`/flash on|off`).
  - On Windows the flash is triggered from the engine callback with
    `FlashWindowEx` (`src/ui/flash.rs`), because eframe pauses the UI loop
    while the window is minimized. Tested on real Windows PCs: works while
    minimized.
  - A notification sound was tried and removed (too harsh, and it only
    played once the window was reopened).

## Work in progress: branch `feature/voice` (prototype)

1-to-1 voice calls (`/call`, `/answer`, `/hangup`, `/mute`, `/devices`,
`/mic`, `/speaker`; call bar with buttons and level meters), `src/voice/`:
cpal capture/playback → sonora (echo cancellation, noise suppression, AGC)
→ Opus 32 kbit/s **constant bitrate** (VBR would leak speech patterns) →
QUIC datagrams on the existing iroh connection. Call signaling uses new
control frames, so `PROTOCOL_VERSION` is 2 (v1 peers get a clear "update"
message). Building needs CMake (Opus). Tested on two real Windows PCs:
calls work with no echo. Device resets (e.g. toggling the mic in Windows)
re-open the stream instead of ending the call; `/voicetest` checks the
mic and speakers locally. A user who hears themselves in headphones has
Windows "Listen to this device" or headset sidetone on (not the call).

## Invite codes (branch `fix/pair-requests`, 0.1.2; merged into `feature/voice`)

Per-device 12-character invite codes (`safety::invite_code`, 60 bits from
the public key). `/invite CODE` pairs only that device, automatically;
plain `/invite` shows each request's code and refuses ambiguous
`/accept NAME`. The code is always shown at the bottom-left.

## Next steps on a Mac

1. Setup: install Xcode Command Line Tools (`xcode-select --install`) and
   Rust (rustup), then `cargo test`.
2. Build and try the real app: `cargo run --release`, and the installer
   with `cargo install cargo-packager --version 0.11.8 --locked` then
   `cargo packager --release --formats app,dmg`.
3. Test on the real Mac: Gatekeeper "Open Anyway" flow, Keychain prompt,
   Chinese font rendering, chat with a Windows PC.
4. Implement the minimized-window alert for macOS (Dock bounce). Today
   `TaskbarFlash` is a no-op outside Windows, and the UI-side
   `RequestUserAttention` only runs while the window is not minimized.
   macOS requires the Dock call on the main thread.
5. Update `README.md` (macOS install steps) and `PROGRESS.md` with results.

## Checks before every commit

```bash
cargo fmt --check
```

```bash
cargo clippy --all-targets -- -D warnings
```

```bash
cargo test
```
