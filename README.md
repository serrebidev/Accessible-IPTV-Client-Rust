# Accessible-IPTV-Client-Rust

Rust port of Accessible-IPTV-Client. Feature-complete at v0.2.0 with a
graphical interface and user experience matching the Python client.
The Python app remains the shipping product; this port is the Rust
successor, released in parallel.

## What it does

- Browse playlists from local files, remote URLs, Xtream Codes and
  Stalker Portal accounts, with a Playlist view scope, a Categories
  tree, favorites and a channel context menu.
- Built-in player driven by libVLC when VLC is installed (system
  media control as fallback), plus external players (VLC, MPC,
  MPC-BE, Kodi, Winamp, Foobar2000, MPV, SMPlayer, Totem, custom).
  Seek and volume sliders, audio track selection with per-channel
  memory and preferred-track rules, sidecar subtitles with
  screen-reader speech, resume positions and a play history.
- Programme guide from XMLTV files and URLs, stored in a local
  SQLite database: episode descriptions, What's on Now, per-channel
  guide, reminders and catch-up playback and downloads.
- Recording: manual and scheduled recordings (DVR) with padding,
  ten format presets, a configurable minimum-free-space reserve,
  and shutdown-after-recordings with a 60-second countdown.
  Scheduled Stalker recordings resolve a fresh stream link at
  their start time and retry until their window closes.
- Casting to UPnP/DLNA renderers, Roku players and Sonos speakers,
  with a local proxy for channels whose streams need custom
  headers.
- Provider playlists are cached on disk for six hours, with the
  cached copy used (and announced) when a portal is unreachable.
- Check for Updates downloads the new package, verifies its
  SHA-256 and opens it for install.
- Full menus (File, Player, View, Options, Recordings, Help), a
  keyboard shortcuts editor, per-control F1 help, and gettext
  translations.

## Crates

- `crates/iptv-core`: widget-free domain logic — playlists,
  providers, EPG and its SQLite store, playback sessions,
  recording and DVR scheduling, casting, updates, i18n.
- `crates/iptv-app`: the wxDragon desktop application.
- `crates/iptv-vlc`: the libVLC playback engine (dlopen; the app
  runs without VLC installed).

## Building

The project uses Rust **1.97.1** (stable release, also the explicit
MSRV), pinned in `rust-toolchain.toml`. Run Cargo from this checkout
so rustup selects that toolchain. Dependency versions are resolved
by the committed `Cargo.lock`.

On Windows, install the MSVC C++ build tools and Windows SDK, CMake
(on `PATH`), and LLVM/libclang. wxDragon's native build requires
these even for `cargo check`. For the standard LLVM installation,
use this PowerShell session setup:

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
cmake --version
rustc --version
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

The first build downloads/builds wxWidgets through `wxdragon-sys`
and can take several minutes. The core can be checked separately
with `cargo test -p iptv-core` without a GUI toolchain. Run the app
with `cargo run -p iptv-app`.

## Releases

Tagged versions build on Windows, Linux and macOS and publish
platform packages plus an `update-manifest.json` (see
`.github/workflows/release.yml` and `PACKAGING.md`). Latest
release: v0.2.0.

## Evidence

Design and parity records live under `docs/`:

- [the GUI parity audit](docs/gui-parity-audit.md), closed out by
  its 2026-11-01 verification update.
- [the replacement-gate pack](docs/validation/0033-replacement-gate-pack.md):
  the evidence ledger for v0.2.0 readiness, including the
  owner-heard NVDA checklist (issue #86) whose pass remains the
  owner's to record.
- Validation notes `docs/validation/0001` onward document each
  slice with automated evidence kept separate from owner-heard
  evidence.
