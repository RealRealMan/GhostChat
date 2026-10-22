# GhostChat — development

User guide: [`README.md`](README.md) · Spec: [`PRD-v0.3.md`](PRD-v0.3.md) · Design: [`ARCHITECTURE.md`](ARCHITECTURE.md) · Security: [`SECURITY.md`](SECURITY.md) · Status: [`PROGRESS.md`](PROGRESS.md) · Open decisions: [`OPEN-QUESTIONS.md`](OPEN-QUESTIONS.md)

Requires Rust 1.95+ (tested with 1.98.1) and **CMake** (builds the Opus codec for voice calls; Windows: `winget install Kitware.CMake`, macOS: `brew install cmake`). Linux also needs `cmake libgtk-3-dev libxdo-dev libxkbcommon-dev libwayland-dev libdbus-1-dev libasound2-dev`.

Run:

```bash
cargo run --release
```

Two copies on one computer (separate identities):

```bash
cargo run --release -- --profile alice
```

```bash
cargo run --release -- --profile bob
```

Flags: `--wipe-identity` (delete this computer's identity and contacts), `--help`. Debug logging to stderr only: set `GHOSTCHAT_DEBUG=1`.

Checks:

```bash
cargo fmt --check
```

```bash
cargo clippy --all-targets -- -D warnings
```

```bash
cargo test
```

Ignored tests that need the internet or the real OS key store:

```bash
cargo test --test internet -- --ignored --nocapture
```

```bash
cargo test --test ui real_stack -- --ignored --nocapture
```

Headless UI tests save screenshots of every screen to `target/ui-shots/`.

Windows installer (output in `dist/`):

```bash
cargo install cargo-packager --version 0.11.8 --locked
```

```bash
cargo packager --release --formats nsis
```

Publishing a release (all platforms, on GitHub):

1. Set `version` in `Cargo.toml`, commit, and push `main`.
2. Write `.github/release-notes/vX.Y.Z.md`, commit and push it.
3. Tag the commit and push the tag: `git tag -a vX.Y.Z -m "GhostChat vX.Y.Z"` then `git push origin vX.Y.Z`.
4. Actions → **release** → **Run workflow** on `main`, enter the tag. It builds Windows, macOS (Apple Silicon and Intel) and Linux packages and creates or updates the GitHub Release with them and `SHA256SUMS.txt`.
