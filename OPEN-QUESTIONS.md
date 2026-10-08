# Open Questions for the Project Owner

Each item: the question, the default chosen so work could continue, and how
easy it is to change later. None of these blocked the build.

## 1. Safety words: split 6+6 instead of "same 6 words on both screens"
PRD §8 says both screens show the same words. To make "confirm by typing"
meaningful, each screen shows **your** 6 words to read aloud and asks you to
type the **other** 6 words that your contact reads to you (never displayed on
your screen). The randomart is identical on both screens.
- Default: split 6+6 (ARCHITECTURE §6).
- Change cost: small (`src/safety.rs` + verify screen).

## 2. CJK font not bundled
Family members may type Chinese. The app uses the system CJK font
(Microsoft JhengHei/YaHei, PingFang, Noto CJK). Verified on this Windows PC
(screenshots show 你好). Bundling Noto Sans CJK would add ~15–20 MB.
- Default: use system font, fall back to boxes if none.
- Change cost: small (add font file + one line in `ui/theme.rs`).

## 3. Automatic reconnect / presence polling
Presence can only be known by trying to connect. The app dials every allowed
contact every 30 s while there is no session, so two family members who both
open the app get connected without typing `/connect`. n0's relay/DNS sees
these periodic lookups.
- Default: on (`auto_connect` in `settings.json`).
- Change cost: trivial.

## 4. KEY_CHANGED detection is codename-based
Identity = public key, so a "changed key" can only be detected when a new key
claims the codename of an existing VERIFIED contact. A new key with a
*different* codename just looks like a new, UNVERIFIED contact.
- Default: as above, with `INTRUSION WARNING` and connection block.
- Change cost: medium if a different rule is wanted.

## 5. Code signing
MVP ships unsigned (PRD §14). Nothing was purchased. macOS build uses ad-hoc
signing in CI (unverified because no Mac was available).

## 6. Fuzzing on stable Rust
`cargo fuzz` needs nightly, which is not installed here. Parsers are covered
by `proptest` property tests on stable; `fuzz/` targets type-check but have
not been run. Install nightly (`rustup toolchain install nightly`) if you want
to run them locally.

## 7. Desktop shortcut is always created
The default NSIS template only offers a "create desktop shortcut" tick box on
the last page; for non-technical users the shortcut is now always created.
- Change cost: trivial (`packaging/windows/installer.nsi`).

## 8. Optional app PIN on launch (PRD §13 "optional") — not implemented
Identity is protected by the OS account (Credential Manager / Keychain).
A PIN for shared computers was left out of MVP.
- Change cost: medium (reuse the passphrase-vault code path).

## 9. Unverified contacts can chat
As specified in PRD §8, ADDED (unverified) contacts may chat; the UI marks
them red. If you prefer "no chat until verified", it is a one-line change in
`ContactBook::can_connect`.

## 10. Bundle identifier
Installers use the identifier `app.ghostchat.family` (no domain is owned; it
only has to be unique on the machine). Change in `Cargo.toml` before the
first real release, because changing it later creates a second "app" on macOS.
