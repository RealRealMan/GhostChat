# GhostChat — Architecture (Phase 0)

Source of truth: `PRD-v0.3.md`. This document records every Phase 0 decision
(PRD §18 Phase 0 and §20) with its rationale and trade-off. Dependency versions
were verified with `cargo search` / `cargo info` on 2026-10-08.

---

## 1. Dependency baseline

| Purpose | Crate | Version | Notes |
|---|---|---|---|
| P2P transport | `iroh` | **1.3.0** (pinned `=1.3.0`) | 1.x stable line; QUIC via n0's `noq` fork, TLS 1.3, raw public keys |
| Async runtime | `tokio` | 1.x | required by iroh |
| Window + UI | `eframe` / `egui` | 0.36.2 | default wgpu renderer |
| File pickers | `rfd` | 0.17 | native open/save dialogs |
| OS secure store | `keyring` | 4.2.0 (`v1` feature) | Windows Credential Manager, macOS Keychain, Linux Secret Service |
| At-rest encryption | `chacha20poly1305` | 0.11 | XChaCha20-Poly1305 AEAD (RustCrypto) |
| Passphrase KDF | `argon2` | 0.6 | Argon2id, only for the no-secure-store fallback |
| Hashing / KDF | `blake3` | 1.8 | checksums, safety-word derivation, file integrity |
| Wordlist | `bip39` | 3.0 | only used for its English 2048-word list |
| Wire encoding | `postcard` + `serde` | 1.1 | compact, schema-by-code, no_std-grade parser |
| Text encoding | `data-encoding` | 2.11 | RFC 4648 base32 for Contact Keys |
| Memory hygiene | `zeroize` | 1.9 | wipe message buffers / key bytes |
| Paths | `directories` | 6.0 | per-OS data/download dirs |
| Tests | `proptest`, `tempfile` | 1.11, 3.x | property tests stand in for fuzzing on stable |

No crypto primitive or protocol is implemented by GhostChat. All encryption is
iroh's QUIC/TLS 1.3 (in flight) and RustCrypto AEAD (at rest).

---

## 2. Transport: iroh 1.3

- **Identity:** the iroh `SecretKey` (Ed25519) *is* the user identity. The
  `EndpointId`/`PublicKey` is the contact identifier everywhere.
- **Endpoint:** `Endpoint::builder(presets::N0)` with our secret key, ALPNs
  `ghostchat/1` (chat) and `ghostchat-pair/1` (pairing).
- **Allowlist enforcement before application data:** iroh 1.x exposes
  `EndpointHooks::after_handshake`, which runs after TLS but *before* the
  connection is handed to the application. Our hook:
  - `ghostchat/1` incoming → accept only if the remote key is a contact in state
    `ADDED` or `VERIFIED`. `KEY_CHANGED`, removed and unknown keys are rejected
    (connection closed with code 403, no stream is ever read).
  - `ghostchat-pair/1` incoming → accept only while the local pairing window is
    open; otherwise rejected silently (no app-level reply).
  - Outgoing connections are only made by our own code to keys we chose.
- **DIRECT / RELAY:** `Connection::paths()` returns the path list with
  `is_selected()` and `is_relay()/is_ip()`. The engine polls the selected path
  once per second and pushes the real value to the status bar.
- **Configurable infrastructure (PRD §6):** `settings.json` has
  `relay_urls: []` and `pkarr_relay_url: null`. Empty / null → n0 defaults
  (`presets::N0`). Otherwise `RelayMode::custom(urls)` and
  `PkarrPublisher::builder(url)` + `PkarrResolver::builder(url)` replace the n0
  services, so a self-hosted `iroh-relay` needs no code change.
- **Relay-only mode (added in Phase 3):** `relay_only: true` builds the
  endpoint with `clear_ip_transports()`, so all traffic goes through the relay
  and contacts never see this computer's IP. Verified against n0
  (`tests/internet.rs::chat_over_n0_relay_only` reports `RELAY`).
- **Version pin:** `iroh = "=1.3.0"`. iroh has changed API on most minor
  releases historically; upgrades must be deliberate.

Trade-off: we depend on n0's free relay + pkarr/DNS discovery. n0 sees IPs,
endpoint IDs and timing (documented in PRD §4). Mitigated by configurability.

App-layer Noise (PRD §20.4): **not in MVP**. iroh's TLS 1.3 with raw public
keys is already mutually authenticated end to end; adding Noise would mean
another handshake and key schedule to get right. Revisit if a post-quantum or
deniability requirement appears. iroh 1.3 already offers a PQ-hybrid key
exchange preset (`prefer-pq-key-exchange` example) which is the cheaper path if
PQ is wanted later.

---

## 3. UI rendering approach (PRD §12, §20.1)

**Decision: eframe/egui directly, styled as a terminal** (bundled monospace
font *Hack* from egui's default fonts, dark background, phosphor colours,
custom-painted effects). Not ratatui-in-egui.

Why:
- Family members in Hong Kong will type Chinese. egui handles OS IME input,
  clipboard paste (needed for Contact Keys) and file drag-and-drop natively.
  A ratatui cell-grid rendered through an egui backend (`egui_ratatui`) would
  re-implement all of that and has weak IME support.
- One mature dependency (egui is widely used, maintained, MIT/Apache) instead
  of two layers.
- Effects (matrix rain, glitch, scanlines, scrolling hex) are simple painter
  calls in egui.

Trade-off: it is not a real character grid, so the "terminal" look is a
styling convention rather than a hard constraint. Acceptable — PRD asks for the
look, not for a terminal.

CJK glyphs: Hack has no CJK. At start-up the app adds the first available
system CJK font as a fallback (Windows `msjh.ttc`/`msyh.ttc`, macOS
`PingFang.ttc`/`Hiragino`/`STHeiti`, Linux Noto CJK). Not bundled, to keep the
installer small. If none is found, CJK shows as boxes (documented).

Status bar isolation (PRD §12): the security bar is a separate egui
`TopBottomPanel` built only from engine state; peer text is only ever placed in
the central message panel as plain `egui` text (no markup interpretation) and
is sanitized first.

---

## 4. Packaging (PRD §14, §20.2)

**Decision: `cargo-packager` 0.11** (CrabNebula; the bundler extracted from
Tauri, maintained).

| OS | Output | Built |
|---|---|---|
| Windows | NSIS `setup.exe` (Start menu + desktop shortcut + uninstaller) | locally + CI |
| macOS | `.app` inside `.dmg`, ad-hoc signed (`signing_identity = "-"`) | built in CI (v0.1.0); not yet installed on a real machine |
| Linux | `.AppImage` + `.deb` | built in CI (v0.1.0); not yet installed on a real machine |

Why not `cargo-dist`: it targets CLI tools (shell/PowerShell installers, MSI
for CLIs) and does not produce `.app`/`.dmg`/AppImage for GUI apps.
Why NSIS over MSI on Windows: per-user install without admin rights, and the
uninstaller can run a custom step (ask before deleting identity/contacts).

The uninstaller removes the program. Identity and contacts are deleted only if
the user ticks the "delete app data" option; the hook runs
`GhostChat.exe --wipe-identity` so the OS secure-store entry is also removed.

Checksums: CI writes `SHA256SUMS.txt` for every package.

---

## 5. Contact Key format (PRD §8, §20)

```
GHOSTKEY1.<CODENAME>.<PUBKEY_B32>.<CHECKSUM_B32>
example:  GHOSTKEY1.NIGHTOWL.ABCD...(52 chars).XYZ1234
```

- `GHOSTKEY1` — format version. Unknown versions are rejected.
- `CODENAME` — validated codename (`[A-Z0-9_-]{3,16}`); never contains `.`.
- `PUBKEY_B32` — 32-byte Ed25519 public key, RFC 4648 base32, no padding
  (52 chars). Uppercase only → survives case-folding chat apps.
- `CHECKSUM_B32` — first 5 bytes of `blake3::derive_key("ghostchat contact key v1 checksum", "GHOSTKEY1" || pubkey || codename)`, base32 (8 chars).
  Detects typos/truncation; it is **not** a security feature (anyone can
  compute it) — authenticity comes from safety-word verification.
- Parser: trims whitespace, removes internal whitespace/newlines (chat apps
  wrap long lines), uppercases, max 200 chars before parsing.
- `.ghostkey` file: the same single line, UTF-8, newline-terminated.
- The format has no field that could carry a secret key; a unit test asserts
  the encoded string never contains the secret key in raw/base32/hex/z32 form.

Trade-off: ~80 chars long. Acceptable because it is pasted, not typed.

---

## 6. Safety words and randomart (PRD §8, §20.3)

Derivation (both sides compute the same thing):

```
(lo, hi) = sort(pubkey_A, pubkey_B)              // byte-wise
digest   = blake3::derive_key("ghostchat safety words v1", lo || hi)   // 32 bytes
words    = 12 × 11-bit indices from digest → BIP39 English wordlist
part 1   = words[0..6]   — read aloud by the owner of `lo`
part 2   = words[6..12]  — read aloud by the owner of `hi`
art      = OpenSSH "drunken bishop" randomart over digest
```

Verification flow (`/verify NAME`):
1. Screen shows the randomart (identical on both screens) and *your* six words
   with "READ THESE ALOUD".
2. The input asks you to type the six words *your contact reads to you*.
   Those words are never shown on your screen, so the user cannot simply copy
   them — this satisfies "confirm by typing, not by pressing one key".
3. Match → `VERIFIED`. Mismatch → error, state unchanged, warning to stop and
   re-pair.

Why BIP39 English: public, well-known, 2048 distinct short words, unique
4-letter prefixes (we accept prefixes, which helps non-native typists).
Strength: 66 bits per direction (an attacker must match both halves; a MITM
produces completely different words on each side).

Trade-off vs PRD text "both screens show the same words": we show the same
randomart, but split the words so typing is meaningful. Recorded in
`OPEN-QUESTIONS.md` for the project owner.

---

## 7. Chat wire format (PRD §9, §16)

Connection: ALPN `ghostchat/1`. Dialer opens one bidirectional **control
stream**. Every frame on it:

```
u32 big-endian length (≤ 80 KiB) || postcard(Frame)
```

```rust
enum Frame {
    Hello     { version: u16, codename: String },   // first frame each way
    Text      { id: u64, body: String },            // body ≤ 64 KiB
    Ack       { id: u64 },                          // delivered
    Codename  { codename: String },                 // rename during session
    FileOffer { id: u64, name: String, size: u64, hash: [u8; 32] },
    FileAccept{ id: u64 },
    FileReject{ id: u64 },
    FileResult{ id: u64, ok: bool },
    Bye,
}
```

- `version` must equal `1`; otherwise the connection is closed with code
  `PROTOCOL_VERSION` and the UI says the peer needs an update.
- Oversized lengths are rejected before allocation. Unknown enum variants fail
  postcard decoding → connection closed with `PROTOCOL_ERROR`.
- File data: after `FileAccept`, the sender opens a **unidirectional stream**:
  `u64 id || raw bytes`. Receiver writes to `<name>.part`, hashes with BLAKE3
  while streaming, enforces the exact offered size, compares hashes, then
  renames. Anything else → `.part` deleted, `FileResult{ok:false}`.
- QUIC already provides confidentiality/integrity per packet; the BLAKE3 hash
  provides end-to-end completeness (truncation, wrong file, disk errors).

Pairing: ALPN `ghostchat-pair/1`, one bi stream, same framing:

```rust
enum PairFrame { Request { version: u16, codename: String },
                 Response { accepted: bool, codename: String } }
```

The requester's public key comes from TLS (`conn.remote_id()`), never from the
payload.

Duplicate connections (both sides dial at once): keep the connection dialed by
the side with the smaller public key; both sides apply the same rule.

---

## 8. Trust model

```
ADDED ──/verify ok──> VERIFIED
VERIFIED ──new key with same codename (via /add or pair request)──> KEY_CHANGED
KEY_CHANGED ──/verify ok (new key)──> VERIFIED
any ──/remove──> (deleted)
```

- Because identity = key, "key changed" can only be detected heuristically: a
  *new* key arrives claiming the codename of an existing **VERIFIED** contact.
  The app shows `INTRUSION WARNING: KEY MISMATCH`. If the user accepts it, the
  contact's key is replaced, state = `KEY_CHANGED`, and all connections with
  that contact are blocked until re-verified. Nothing is accepted silently.
- `ADDED` contacts can chat; UI shows `UNVERIFIED` in red.

---

## 9. Storage (PRD §13, §11)

Data dir: `directories::ProjectDirs("", "", "GhostChat")` (+ `/<profile>` when
`--profile NAME` is used for testing two instances on one machine).

| Item | Where | Protection |
|---|---|---|
| Identity secret key + storage key (64 bytes) | OS secure store, service `GhostChat` (`GhostChat-<profile>`), user `master` | OS |
| Contacts | `contacts.bin` | XChaCha20-Poly1305 with the storage key |
| Settings | `settings.json` | plain (theme, codename, relay config) |
| Fallback when no secure store | `vault.bin` | Argon2id(passphrase) → XChaCha20-Poly1305 |
| Received files | `~/Downloads/GhostChat/` | none (user's files) |

Identity lookup order: `vault.bin` exists → passphrase; else OS store. If the
OS store is unreachable on a **first run**, a passphrase vault is created. If
it is unreachable but an identity already exists (codename or contacts on
disk), GhostChat shows an error instead of silently creating a new key — a new
key would look like a key change to every contact (fixed in Phase 7).

No message database, no log file. Debug logging is opt-in via
`GHOSTCHAT_DEBUG=1`, goes to stderr only, and the code never logs message
content, keys or file content.

---

## 10. Process layout

```
main thread: eframe UI (App)  ──Command──►  engine (tokio runtime, bg thread)
                             ◄──Event────   Endpoint, sessions, pairing, files
```

- `Engine` owns the iroh `Endpoint`, contact book (shared `Arc<Mutex<_>>`
  with the hook), sessions and transfers.
- UI holds only display state: sanitized messages (zeroized when cleared).
- Panic (`F12` / `/panic`): UI sends `Panic`, engine closes all connections
  and the endpoint, UI zeroizes all buffers, plays a short glitch and exits.
  "Panic also wipes identity" defaults **off** (PRD §20.5).
- Crash dumps: Unix `setrlimit(RLIMIT_CORE, 0)`, Linux
  `prctl(PR_SET_DUMPABLE, 0)`; Windows `SetErrorMode(SEM_NOGPFAULTERRORBOX)`.
  Windows WER LocalDumps are a system policy we do not change (documented).

---

## 11. Module map

```
src/main.rs          args (--profile, --wipe-identity), hardening, start UI
src/lib.rs
src/codename.rs      validation
src/sanitize.rs      peer text + filename sanitization
src/contact_key.rs   Contact Key encode/decode
src/safety.rs        safety words + randomart
src/contacts.rs      Contact, TrustState, ContactBook transitions
src/storage.rs       secret store, encrypted contacts, settings, vault
src/proto.rs         frame codec (chat + pair)
src/engine.rs        iroh endpoint, hook, sessions, pairing, file-transfer glue
src/transfer.rs      file hashing, streaming, .part handling, unique names
src/hardening.rs     crash-dump suppression, opt-in stderr debug logging
src/ui/mod.rs        App + screens (boot, codename, unlock, main, verify, panic, error)
src/ui/core.rs       main screen state, slash-command handling, panels
src/ui/commands.rs   slash-command parser
src/ui/effects.rs    scanlines, glitch, typewriter, Matrix rain, handshake viz
src/ui/theme.rs      palettes, fonts (Hack + system CJK fallback)
```

Testing layers: unit tests per module; `tests/chat.rs`, `pairing.rs`,
`files.rs`, `malicious.rs` run two engines over loopback (iroh with relay and
discovery disabled, `MemoryLookup`); `tests/ui.rs` drives two real `App`
instances headlessly with `egui_kittest` (keyboard events, wgpu screenshots);
`tests/internet.rs` and `ui.rs::real_stack_*` use the real n0 network and OS
key store (ignored by default); `tests/fuzz_props.rs` is property-based
fuzzing.

---

## 12. Open decisions resolved here

| PRD §20 item | Decision |
|---|---|
| 1. UI rendering | eframe/egui styled terminal (§3) |
| 2. Packaging | cargo-packager: NSIS / DMG / AppImage+deb (§4) |
| 3. Safety words | BIP39 English, BLAKE3 derive_key, 2×6 words split by key order (§6) |
| 4. App-layer Noise | Not in MVP; PQ via iroh preset is the preferred later path (§2) |
| 5. Panic | `F12` + `/panic`; wipe-identity default **off** (§10) |
| (extra) iroh version | `=1.3.0` (§2) |
| (extra) Contact Key | `GHOSTKEY1.<CODENAME>.<B32 key>.<B32 checksum>` (§5) |
| (extra) Wire format | length-prefixed postcard frames, versioned Hello (§7) |

No blocking issue found → proceeding to Phase 1 per the project owner's authorization.
