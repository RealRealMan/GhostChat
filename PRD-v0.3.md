# GhostChat — Product Requirements Document

**Version:** 0.3
**Supersedes:** v0.2 (`PRD.md`, kept for reference)
**Platform:** Windows / macOS / Linux (desktop only)
**Status:** Architecture / MVP Planning

---

## 1. What changed from v0.2

v0.2 described a public, general-purpose secure messenger. v0.3 narrows the product to its real purpose:

> A private, end-to-end encrypted, live chat tool for **one family**, with a movie-style hacker interface, that non-technical family members can install by double-clicking.

| v0.2 | v0.3 |
|---|---|
| Public discovery server, rotating IDs | Removed. iroh dials by public key |
| Custom NAT traversal + relay design | Removed. Provided by iroh |
| Self-hosted infrastructure | None. No paid servers |
| PQ hybrid, Double Ratchet | Deferred (not MVP) |
| CLI-first (`ghostchat send ...`) | App-first. Everything inside the UI |
| Offline mailbox, multi-device | Out of scope |
| — | Codenames (nicknames) |
| — | One-click installer for each OS |
| — | Hacker UX section |

---

## 2. Goals

1. Messages and files are end-to-end encrypted. Nobody in between (ISP, relay, platform) can read them.
2. Only family members whose keys were exchanged and verified can connect.
3. No message history on disk. Closing the app leaves no conversation behind.
4. Zero running cost: no servers to rent.
5. A non-technical user can install it by downloading one file and double-clicking.
6. Looks and feels like a hacker terminal from a movie.

## 3. Non-Goals

- Offline / asynchronous messaging (both people must be online).
- Mobile apps.
- Multiple devices per person.
- Group chat (MVP is 1-to-1; may be added later).
- Anonymity. GhostChat hides **content**, not the fact that two IPs talked.
- Protection against a compromised computer (malware, keyloggers, someone at your keyboard).

---

## 4. Threat Model (short)

**Protect against:**

- Messaging platforms, ISPs, Wi-Fi owners and relay operators reading message or file content.
- Someone impersonating a family member.
- Tampered, replayed or injected packets.
- Strangers connecting to a user.
- Conversation content being recoverable from disk after the app closes.

**Accepted / documented limitations:**

- The relay/discovery operator (n0, see §6) can see IP addresses, node IDs and connection timing.
- Peers connected directly can see each other's IP address (acceptable: they are family).
- The app binary, the encrypted identity key and the contact list remain on disk.
- Files received are saved to disk in plaintext; the user is responsible for them.
- OS swap, crash dumps and memory forensics cannot be fully controlled.

---

## 5. Architecture

```text
┌──────────────────────────────────────────────┐
│  UI (hacker terminal look, own window)       │
├──────────────────────────────────────────────┤
│  GhostChat Core                              │
│   Identity · Codename · Contacts · Chat ·    │
│   File transfer · Wipe/Panic                 │
├──────────────────────────────────────────────┤
│  iroh  (QUIC + TLS 1.3, dial-by-public-key,  │
│         hole punching, relay fallback)       │
└──────────────────────────────────────────────┘

  Alice ──── direct P2P (when NAT allows) ──── Bob
     └────── n0 public relay (ciphertext only) ──┘
```

Language: **Rust**. One self-contained binary per OS, no runtime to install.

---

## 6. Transport: iroh

Use the iroh library for all networking.

- Each installation's iroh node key (Ed25519) **is** the user's identity key.
- Connections are QUIC with TLS 1.3, authenticated by both peers' public keys.
- iroh handles discovery, hole punching and relay fallback.
- Default discovery and relay servers are the free public ones run by n0.

Requirements:

- Pin the API to a specific iroh version; verify the current stable release before starting.
- Relay and discovery servers must be **configurable**, so a self-hosted `iroh-relay` (home PC or a free-tier VM) can replace n0 without code changes if n0's free service changes.
- Accept incoming connections **only** from public keys in the local contact list. Drop everything else before any application data is exchanged.
- Use a GhostChat-specific ALPN string with a version, e.g. `ghostchat/1`.

App-layer encryption on top of iroh (e.g. Noise) is **not** in MVP; iroh's transport is already authenticated end-to-end. It may be added later as defence-in-depth.

Do not implement cryptographic primitives or protocols manually.

---

## 7. Identity & Codename

On first launch, the app generates an identity key locally and asks for a **codename**.

### Codename rules

- 3–16 characters, `A–Z`, `0–9`, `-`, `_`. Displayed in uppercase.
- Examples: `NIGHTOWL`, `GHOST-01`, `ZER0COOL`.
- Can be changed later in settings.

### Important properties

- There is **no registration server**. A codename is not globally unique and is not an identity. The public key is the identity.
- Your codename is included in your Contact Key and sent to contacts during the session handshake.
- When a contact's codename changes, show it: `NIGHTOWL is now known as GHOST-01`.
- If two contacts share a codename, disambiguate locally (e.g. `NIGHTOWL·7F3A`).
- The receiver may override any contact's display name locally.
- Codenames from peers are untrusted input: validate them against the rules above and reject anything else.

---

## 8. Contact Key, Pairing & Verification

### Contact Key

A short text string (and optional `.ghostkey` file) containing:

- Format version
- Public key
- Codename
- Checksum

It never contains the private key. Automated tests must verify this.

It is public, so it may be sent through any channel (USB, email, even WhatsApp).

### Pairing (remote, one-way invite)

Pairing is done remotely; no physical meeting is assumed. To keep it simple for non-technical users, only **one** person pastes a key:

1. The inviter (e.g. you) runs `/invite`. This shows their Contact Key and opens a **pairing window** (e.g. 10 minutes).
2. The inviter sends the Contact Key through an existing channel of their choice (e.g. Signal).
3. The invitee pastes it with `/add`. Their app connects to the inviter and sends its own public key and codename.
4. The inviter sees `INCOMING PAIR REQUEST: NIGHTOWL` and accepts or rejects it.
5. Both sides are now contacts (ADDED, unverified).

Rules:

- Pair requests are accepted only while the inviter's pairing window is open. Outside it they are dropped silently.
- A pair request never grants VERIFIED status by itself.

### Verification

The Contact Key and pair request travel over channels GhostChat does not control, so each contact must be verified once:

- Both screens show the same **safety words** (e.g. 6 words derived from both public keys) and a **randomart** image (SSH-style).
- Verify over a **voice or video call** where you recognise the person, by reading the words aloud. Do not verify by sending the words as text over the same channel that carried the Contact Key.
- The user confirms by typing or selecting the words, not by pressing a single "verify" key.

### Trust states

```text
ADDED (unverified) ──verify──> VERIFIED
VERIFIED ──key changed──> KEY_CHANGED (connection blocked until re-verified)
any ──remove──> REMOVED
```

- Unverified contacts may connect, but the UI shows `UNVERIFIED` prominently.
- A changed key is never accepted silently.

---

## 9. Chat

- Live 1-to-1 text chat between two online contacts.
- Show contact presence: `ONLINE` / `OFFLINE` / `CONNECTING`.
- Sending to an offline contact fails with a clear message. Nothing is queued.
- Show per-message delivery status (sent / delivered).
- Maximum message size (e.g. 64 KB).
- Messages exist only in memory. Closing a chat or the app destroys them (`zeroize` where practical).
- Transport line visible at all times: `E2EE · DIRECT` or `E2EE · RELAY`.

---

## 10. File Transfer

- Sender selects a file. The receiver must **accept** before any data flows.
- Size limit (configurable, default e.g. 2 GB).
- Streamed in chunks over the encrypted connection with integrity verification.
- Filename from the peer is sanitized: strip path components, reject `..` and reserved names (e.g. Windows `CON`, `NUL`).
- Saved to a GhostChat downloads folder. The UI states that the received file is now a normal file on disk.
- Warn on executable file types (`.exe`, `.bat`, `.ps1`, `.sh`, `.app`, ...).
- Partial or corrupted transfers are deleted, not kept.

---

## 11. "No Traces" — Exact Definition

GhostChat **does**:

- Keep messages only in memory. No message database, no chat log.
- Write no application logs by default. Debug logging is opt-in and never contains message content, keys or file content.
- Render in its own window (§12), so no terminal scrollback keeps the conversation.
- Clear sensitive memory on exit and on panic, where practical.
- Disable crash dumps / core dumps for the process where the OS allows it.

GhostChat **does not** hide:

- That GhostChat is installed.
- The encrypted identity key and the contact list on disk.
- Received files.
- Network metadata (IPs, timing) visible to ISPs and the relay operator.

Product text must never claim "100% untraceable", "zero traces" or "anonymous".

### Panic

- Hotkey (e.g. `F12`) or command `/panic`.
- Closes connections, wipes in-memory messages and session state, plays a short glitch effect, exits.
- Optional setting "panic also wipes identity": deletes identity key and contacts (requires re-pairing).

---

## 12. UI & Hacker UX

### Windowing decision

Family members double-click an icon; they must not need to understand terminals, fonts or shells.

**Decision: GhostChat opens its own window that looks like a terminal**, rather than running inside the OS terminal. Benefits:

- Same look on all three OSes, bundled monospace font, guaranteed box-drawing and colors.
- No terminal scrollback (supports §11).
- Normal app behaviour: icon, window, close button.

Phase 0 must choose the rendering approach (e.g. ratatui with a GUI backend such as an egui-based one, or another mature option), verifying crate maturity first.

### Hacker UX features

- **Boot sequence**: `[ OK ] Loading identity...` `[ OK ] Opening secure channel...`
- **Codename screen** on first launch with typewriter effect.
- **Handshake visualization** when connecting: steps, scrolling hex, then `SECURE CHANNEL ESTABLISHED`.
- **Randomart + safety words** for verification.
- **Themes**: green phosphor (default), amber CRT, ice blue.
- **Idle screen**: Matrix-style rain.
- **Identity warning**: red glitch effect, `INTRUSION WARNING: KEY MISMATCH`.
- **Panic**: screen glitch, wipe animation, exit.
- Optional retro effects: scanlines, flicker (can be turned off).

### Rules for effects

- Scrolling hex and "data" animations use public or random data only. **Never display private keys or session keys.**
- The security status bar (`E2EE`, `VERIFIED`, `DIRECT/RELAY`) must reflect real state, never a decoration.
- All peer-supplied text (messages, codenames, filenames) is sanitized: strip control characters, ANSI/escape sequences and bidi override characters.
- The status bar is drawn in a screen region peer content cannot write to.

### Interaction

Everything is done inside the app. Keyboard-first, slash commands, with on-screen hints for non-technical users:

```text
/invite    show your Contact Key and open a pairing window
/add       add contact (paste Contact Key or open .ghostkey)
/verify    verify a contact with safety words
/connect   connect to a contact
/send      send a file
/codename  change your codename
/mykey     show your Contact Key, randomart and safety words
/theme     change theme
/panic     wipe and exit
/help
```

No separate CLI is required for MVP.

---

## 13. Storage

Persist only:

- Identity private key: in the OS secure store (Windows Credential Manager/DPAPI, macOS Keychain, Linux Secret Service), via a maintained crate such as `keyring`. If no secure store is available (some Linux setups), fall back to an encrypted file protected by a user passphrase.
- Contact list (public keys, codenames, trust state): encrypted at rest with a key held in the OS secure store.
- Settings (theme, codename, relay config).

Optional: app PIN on launch, for shared computers.

---

## 14. Installation & Distribution

Target: **download one file → double-click → installed, with a desktop/Start menu icon.** Nothing else to install.

| OS | Package | Notes |
|---|---|---|
| Windows 10/11 x64 | `.msi` or setup `.exe` | Start menu + desktop shortcut, uninstaller |
| macOS (Apple Silicon + Intel) | `.dmg` with `.app` | Drag to Applications |
| Linux x64 | `.AppImage` (+ `.deb` optional) | AppImage needs no install |

Requirements:

- Single self-contained binary inside each package. iroh, fonts and assets are bundled.
- Build all packages in CI (e.g. GitHub Actions with a tool such as cargo-dist or cargo-packager; verify current maintained options).
- Uninstaller removes the app; it asks before deleting identity and contacts.
- Publish SHA-256 checksums for each package.

### Code signing (no-budget decision)

Signing certificates cost money (Apple Developer ID ~USD 99/year, Windows signing has ongoing cost). MVP ships **unsigned**, which means:

- Windows SmartScreen shows "Windows protected your PC": click **More info → Run anyway**.
- macOS Gatekeeper blocks first launch: on macOS 15+ allow it in **System Settings → Privacy & Security → Open Anyway** (older versions: right-click → Open). macOS binaries must still be ad-hoc signed (free) to run on Apple Silicon.

These warnings are triggered by the "downloaded from the internet" mark (Windows Mark-of-the-Web, macOS quarantine attribute). Installers delivered remotely (download or messaging app) should be expected to carry it.

Mitigation (remote install):

- Ship a one-page **picture guide** per OS showing exactly which buttons to click.
- Walk the family member through the first install on the same voice/video call used for verification (§8).
- Send the installer only through a trusted channel (e.g. Signal) or an HTTPS release page, so it cannot be swapped in transit.

Updates are manual in MVP.

Future option: if the repository is open source, free signing programmes for open-source projects (e.g. SignPath Foundation for Windows) can be evaluated.

---

## 15. First-Run Flow

```text
Double-click GhostChat
   │
   v
Boot sequence animation
   │
   v
"ENTER YOUR CODENAME" → NIGHTOWL
   │
   v
Identity generated (key stored in OS secure store)
   │
   v
Show Contact Key + randomart + "share this with your family"
   │
   v
Inviter: /invite → send Contact Key via Signal
Invitee: /add → inviter accepts pair request
   │
   v
Voice/video call → /verify with safety words
   │
   v
/connect → SECURE CHANNEL ESTABLISHED
```

---

## 16. Security Requirements Summary

- No custom cryptography; use iroh's transport and maintained crates.
- Identity = public key, never IP or codename.
- Only known contacts may connect; strangers are dropped before application data.
- Key changes block the connection until re-verified.
- No message content on disk, in logs, or in terminal scrollback.
- All peer input is untrusted: size limits, parsers fuzzed, text sanitized, filenames sanitized.
- Never display secret keys in UI effects.
- Protocol messages are versioned; unknown versions are rejected cleanly.

---

## 17. Testing

Unit:

- Contact Key encode/decode, checksum, and "no private key inside" test.
- Codename validation and sanitization.
- Safety words / randomart determinism (same keys → same output on both sides).
- Text sanitization (ANSI, control, bidi characters).
- Filename sanitization (path traversal, reserved names).
- Trust state transitions, including key change.

Integration (two app instances on one machine and across networks):

- Connect, chat, disconnect.
- Direct and relay paths.
- Unknown key is rejected.
- Changed key is blocked.
- Offline contact handling.
- File transfer: accept, reject, corrupted, truncated.
- Panic wipes state and exits.

Fuzzing:

- Contact Key parser.
- Chat message and file-transfer message parsers.

Manual:

- Fresh install on clean Windows, macOS and Linux machines by a non-technical tester.

CI: `rustfmt`, `clippy`, tests, `cargo audit`, package builds for all three OSes.

---

## 18. Development Phases

Each phase should leave a working, demo-able app.

**Phase 0 — Architecture (no code)**
Verify current iroh version and API; choose UI rendering approach (§12); choose packaging tool (§14); define Contact Key format, safety-word derivation and chat wire format. Stop for review.

**Phase 1 — Walking skeleton**
Two instances chat over iroh using hard-coded keys, in a minimal hacker-styled window.

**Phase 2 — Identity & contacts**
First-run codename, key in OS secure store, Contact Key, add/verify/remove, trust states, allowlist enforcement.

**Phase 3 — Chat polish**
Presence, delivery status, sanitization, status bar, no-traces behaviour, panic.

**Phase 4 — Packaging**
Installers for all three OSes from CI; install test on clean machines.

**Phase 5 — File transfer**

**Phase 6 — Hacker UX**
Boot sequence, handshake visualization, themes, Matrix idle screen, glitch effects.

**Phase 7 — Hardening**
Fuzzing, dependency audit, threat-model review.

---

## 19. MVP Acceptance Criteria

1. A non-technical user installs GhostChat on Windows, macOS or Linux by double-clicking one file received remotely, following the picture guide for unsigned-app warnings.
2. On first launch the user chooses a codename and an identity is created.
3. Two users exchange Contact Keys and verify them with safety words.
4. Two verified users on different home networks can chat live without knowing each other's IP and without any paid server.
5. Unknown keys cannot connect. Changed keys are blocked until re-verified.
6. The relay cannot read message or file content.
7. After closing the app, no conversation content exists on disk or in terminal scrollback.
8. Security status (E2EE, VERIFIED, DIRECT/RELAY) is always visible and accurate.
9. Files can be sent with receiver consent and integrity verification.
10. Panic wipes session state and exits immediately.

---

## 20. Open Decisions for Phase 0

1. UI rendering approach for an own-window terminal look (§12).
2. Packaging tool for the three OSes (§14).
3. Safety-word wordlist and derivation.
4. Whether to add app-layer Noise encryption on top of iroh later.
5. Default panic hotkey and whether "wipe identity on panic" defaults to off (recommended: off).
