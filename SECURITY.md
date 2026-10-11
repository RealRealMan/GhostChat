# GhostChat — Threat-Model Review (Phase 7)

Reviewed against PRD-v0.3 §4 (threat model), §11 (No Traces) and §16
(security requirements). "Test" names refer to files under `tests/` or unit
tests in `src/`.

## 1. Protected threats

| Threat (PRD §4) | Mitigation | Evidence |
|---|---|---|
| ISP / Wi-Fi / relay reads content | All traffic is iroh QUIC + TLS 1.3 with raw Ed25519 keys; relays forward ciphertext only. No custom crypto. | `tests/internet.rs` (direct + relay-only paths over n0) |
| Someone impersonates a family member | Identity = public key. Contact Keys carry the key; pairing takes the requester's key from TLS, never from the payload. Safety words (split 6+6, BIP39) + randomart, verified by voice; user must *type* the words the other person reads (never shown on their own screen). | `src/safety.rs` tests, `tests/ui.rs::first_run_pair_verify_chat` |
| Tampered / replayed / injected packets | QUIC AEAD + TLS 1.3 handle per-packet integrity and replay. App frames are length-checked before allocation; unknown variants close the connection; protocol version checked in Hello. | `src/proto.rs` tests, `tests/malicious.rs::{garbage_frame_closes_session, unknown_protocol_version_is_refused}`, `tests/fuzz_props.rs` |
| Strangers connecting | `EndpointHooks::after_handshake` rejects any key that is not an ADDED/VERIFIED contact before a stream is accepted. Pairing ALPN is accepted only while `/invite` is open; at most 8 pending requests. | `tests/chat.rs::unknown_key_is_rejected`, `tests/pairing.rs::pair_request_outside_window_is_dropped_silently` |
| Key change accepted silently | A new key claiming a VERIFIED contact's codename → `INTRUSION WARNING`, explicit confirmation, state `KEY_CHANGED`, all connections blocked until re-verified; old key removed. | `tests/pairing.rs::changed_key_is_blocked_until_reverified`, `src/contacts.rs` tests |
| Conversation recoverable from disk | No message store, no log file. Messages are only in memory, zeroized on drop / `/clear` / exit / panic. Own window (no terminal scrollback). Crash dumps suppressed where the OS allows. | `tests/ui.rs` (clear + panic), `src/hardening.rs` |
| Hostile peer input | Text: ANSI/OSC/CSI, C0/C1 controls, bidi and zero-width chars stripped. Codenames validated (invalid ones ignored). Filenames: last component only, `..` and Windows reserved names rejected, invalid chars replaced, length capped; executables flagged. Status bar is built only from engine state in its own panel. | `src/sanitize.rs` tests, `tests/malicious.rs`, `tests/fuzz_props.rs` |
| Files written without consent / corrupted files kept | No data stream is read unless the user accepted that file id. Size and BLAKE3 hash must match the offer; otherwise the `.part` file is deleted. Size limit auto-rejects. | `tests/files.rs`, `tests/malicious.rs::unsolicited_data_stream_is_not_written` |
| Secret keys shown in effects | Effects use `rand` output or the peer's **public** key only. The secret key is never formatted anywhere. | Code review of `src/ui/effects.rs`; Contact Key test `never_contains_private_key` |
| Voice call content (0.2, experimental) | Audio is sent as QUIC datagrams inside the same authenticated iroh connection, so it has the same end-to-end encryption as chat. Opus runs at a **constant bitrate** with DTX off, so packet sizes do not reveal speech patterns; a muted microphone still sends silent packets. Calls need an explicit answer; the microphone is opened only while a call is active and closed when it ends, on disconnect and on panic. | `src/voice/pipeline.rs::encode_decode_through_jitter_buffer` (all packets equal size), `tests/chat.rs::voice_call_*` |
| Hostile voice packets | Packets are length- and version-checked before the Opus decoder; packets of another call id are dropped; the jitter buffer is bounded (10 packets). | `src/voice/pipeline.rs` tests |

## 2. Accepted / residual risks

1. **Metadata.** n0 (relay + pkarr/DNS discovery) sees IPs, endpoint IDs and
   timing; contacts on a direct path see each other's IP. Mitigations:
   relay/discovery are configurable (self-host `iroh-relay`), and
   `relay_only: true` in `settings.json` hides your IP from contacts.
2. **KEY_CHANGED is codename-based.** A malicious new key that uses a
   *different* codename appears as a new, UNVERIFIED contact (red `[?]`, red
   `UNVERIFIED` in the status bar). Safety-word verification is the defence.
3. **Unverified contacts can chat** (PRD §8). They are marked prominently.
4. **Contacts can flood.** A paired contact can send many messages/offers;
   channels are unbounded except file offers (16 pending). Accepted because
   contacts are family; revisit if needed.
5. **Local compromise is out of scope** (PRD §3): malware running as the user
   can read the OS secure store entry, the clipboard (Contact Keys are public
   anyway) and process memory.
6. **Memory hygiene is best effort.** egui keeps its own copies of rendered
   text (galley cache) and the IME/clipboard are OS-controlled; these are not
   zeroized. Swap/hibernation files can contain message text.
7. **Crash dumps on Windows:** the process suppresses the WER dialog, but a
   system-wide WER `LocalDumps` policy (set by an administrator) could still
   write a dump. Changing it would modify system settings, so we do not.
8. **Received files are plaintext** in `Downloads/GhostChat` and stay after
   panic/uninstall (PRD §4, stated in the UI when a file arrives).
9. **Settings file is plaintext** (codename, theme, relay config). Contacts
   and keys are encrypted / in the OS store.
10. **Unsigned installers** (PRD §14): users must trust the delivery channel;
    SHA-256 checksums are produced in CI.
11. **Pairing window:** anyone who gets the Contact Key while the window is
    open can send a request; it still needs an explicit `/accept`, and the
    result is UNVERIFIED until safety words match.
12. **No post-quantum protection** for recorded traffic (PRD §20.4 deferred).
13. **Voice call metadata.** The traffic pattern shows *that* a call is
    happening and how long it lasts (about 50 packets per second).
14. **Opus is C code.** libopus decodes data from the peer. It is the
    reference implementation used by browsers and WebRTC and is built with
    its hardening flags, but it is not memory-safe Rust; keep the `opus`
    dependency up to date.
15. **Microphone access log.** Windows/macOS privacy settings record that
    GhostChat used the microphone (not what was said).

## 3. Dependency audit

`cargo audit` result is recorded in `PROGRESS.md` (Phase 7). iroh is pinned
to `=1.3.0`; other dependencies are the latest stable versions as of
2026-10-08.

## 4. Fuzzing

- Stable: `tests/fuzz_props.rs` (proptest, 4000 cases per property) covers the
  Contact Key parser, chat/pair frame decoders, the framed stream reader,
  text/filename sanitizers, codename validation and safety-word checks.
- Nightly: `fuzz/` contains cargo-fuzz targets (`contact_key`, `chat_frame`,
  `pair_frame`). They type-check on stable but have **not been run** locally
  (no nightly toolchain); CI runs them for 60 s each.

## Reporting

This is a private family project. Report issues directly to the project owner.
