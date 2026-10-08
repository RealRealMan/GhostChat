# GhostChat — Progress

Status as of 2026-10-08 (autonomous build run). Source of truth: PRD-v0.3.md.

| Phase | Status | Commit |
|---|---|---|
| 0 Architecture | Done — `ARCHITECTURE.md`, `OPEN-QUESTIONS.md` | `ebc10b6` |
| 1 Walking skeleton | Done | `d9b7dd9` |
| 2 Identity & contacts | Done | `0a6ac6a` |
| 3 Chat polish / panic / no-traces | Done | `6ad322f` |
| 4 Packaging | **Partly done** — Windows installer built and tested locally; macOS/Linux only as CI workflow (not run) | `f975adb` |
| 5 File transfer | Done | `55b2f3c` |
| 6 Hacker UX | Done | `e5a159c` |
| 7 Hardening | Done for stable-Rust scope; nightly fuzzing not run | `31ee205` |

## Verified (actually run on this Windows 11 machine)

- `cargo fmt --check`, `cargo clippy -- -D warnings` and
  `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 92 tests pass (55 unit, 6 chat, 5 pairing, 7 files,
  6 hostile-peer, 9 property-fuzz, 4 headless UI).
- Ignored tests, run manually once:
  - `tests/internet.rs`: two engines over **n0's public relay + DNS discovery**:
    chat works; relay-only mode reports `RELAY` correctly.
  - `ui.rs::real_stack_os_store_and_n0`: two full apps (headless) with the
    **real Windows Credential Manager** and n0: first run, invite/add/accept,
    chat. Test credentials were deleted afterwards (`cmdkey /list` clean).
  - `storage::os_secure_store_roundtrip`: Credential Manager read/write/delete.
- Headless UI tests render every screen to `target/ui-shots/*.png`; reviewed
  visually (boot, codename, invite, pair request, verify, chat incl. Chinese
  text, file offer, handshake animation, Matrix idle, panic, 3 themes).
- Real window: the release exe opens its own window with icon, no console.
- Windows installer `dist/ghostchat_0.1.0_x64-setup.exe` (NSIS, per-user):
  silent install → files, Start-menu + desktop shortcut, Add/Remove Programs
  entry present → installed app launches → `--wipe-identity` works →
  silent uninstall removes files, shortcuts and registry entry. Final build
  (9.7 MB) SHA-256: `E5C55E6A1E4E1665EEE33E0F4B9F671B9440DA617518FF8F3B7FBA5ADECDED65`
  (also in `dist/SHA256SUMS.txt`; `dist/` is not committed).
- `cargo audit`: **0 vulnerabilities**; 3 "unmaintained" warnings, all
  transitive (`atomic-polyfill` via postcard/heapless, `paste` via iroh's
  Linux netlink deps, `ttf-parser` via winit's Wayland decorations).
- Fuzz targets in `fuzz/` type-check on stable.

## Not verified / known gaps

- **Two different networks / NAT traversal across the internet**: only tested
  with both peers on this one PC (n0 relay path and direct path both seen).
  Needs a real test between two homes.
- **macOS and Linux**: nothing was built or run. `release.yml`/`ci.yml` are
  written but never executed (no GitHub remote, per instructions). The macOS
  ad-hoc signing, DMG, AppImage and .deb are untested; Keychain prompt
  behaviour unverified; Linux Secret Service fallback only unit-tested.
- **Uninstaller "also delete identity" tick box**: the NSIS hook was added but
  only the silent (unticked) path was tested; ticking it in the GUI
  uninstaller is untested.
- **Unsigned-app warnings**: README describes SmartScreen / Gatekeeper steps
  as text drawings; no real screenshots (would need a clean machine).
- **Non-technical tester install** (PRD §17 manual test): not done.
- **Typing in the real window** was not automated (synthetic window messages
  don't reach winit); the same UI code is exercised by the headless kittest
  harness instead, which found and fixed a real bug (Enter did not submit).
- **cargo-fuzz** runs need nightly (not installed); CI job unverified.
- Optional app PIN (PRD §13) not implemented (OPEN-QUESTIONS #8).
- Memory wiping is best effort (egui text caches, OS IME/clipboard) — see
  SECURITY.md §2.

## Bugs found and fixed during the build

- Boot screen never left without mouse movement (missing repaint on screen
  change).
- Enter did not submit any input field (`lost_focus` + re-focus pattern);
  replaced with an explicit check, caught by the headless UI test.
- Re-opening `/invite` while already open sent no confirmation event.
- If the OS key store was temporarily unreachable, a new identity would have
  been created silently (looks like a key change to all contacts) — now an
  error screen.
- `✓`/`✗` glyphs missing from the bundled font — replaced with ASCII markers.
