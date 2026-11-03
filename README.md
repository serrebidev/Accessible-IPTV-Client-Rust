# Accessible-IPTV-Client-Rust

Rust port of Accessible-IPTV-Client, released in parallel while the
Python app remains the shipping product. The port has a wxDragon
desktop interface and core implementations, but the
[current feature parity audit](docs/feature-parity-audit.md) records
remaining behavior gaps against Python v1.150.58. The Rust port is
not yet feature-complete against that reference.

## What it does

- Browse playlists from local files, remote URLs, Xtream Codes and
  Stalker Portal accounts, with a Playlist view scope, a Categories
  tree, favorites and a channel context menu.
- Built-in player driven by libVLC (bundled in release packages;
  system media control as fallback), plus external players (VLC, MPC,
  MPC-BE, Kodi, Winamp, Foobar2000, MPV, SMPlayer, Totem, custom).
  Seek and volume sliders, audio track selection and preferred-track
  rules, sidecar subtitles with
  screen-reader speech, resume positions and a play history.
- Programme guide from XMLTV files and URLs, mirrored into a local
  SQLite database: episode descriptions, What's on Now, per-channel
  guide, reminders and catch-up playback and downloads.
- Recording: manual and scheduled recordings (DVR) with padding,
  ten format presets, a minimum-free-space setting (Windows
  enforcement remains a gap),
  and shutdown-after-recordings with a 60-second countdown.
  Scheduled Stalker recordings resolve a fresh stream link at
  their start time and retry until their window closes.
- Casting to UPnP/DLNA renderers and Roku players. Header-protected
  casting has proxy gaps; other Caster protocols remain incomplete.
- Provider playlists are cached on disk for six hours, with the
  cached copy used (and announced) when a portal is unreachable.
- Check for Updates uses ship-shape to verify minisign signatures;
  Windows/Linux self-install and relaunch, while macOS opens the
  verified disk image for manual installation.
- Full menus (File, Player, View, Options, Recordings, Help), a
  keyboard shortcuts editor, per-control F1 help on main controls,
  and gettext support. Bundled translation resources remain incomplete.

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
`.github/workflows/release.yml` and `PACKAGING.md`).

## Evidence

Design and parity records live under `docs/`:

- [the current feature parity audit](docs/feature-parity-audit.md):
  source-linked gaps across major features and smaller behaviors,
  with focused acceptance cases and evidence limits.
- [the historical GUI parity audit](docs/gui-parity-audit.md):
  the original surface inventory and its implementation updates.
  Those updates do not close the newer behavior gaps.
- [the replacement-gate pack](docs/validation/0033-replacement-gate-pack.md):
  the evidence ledger for v0.2.0 readiness, including the
  owner-heard NVDA checklist (issue #86) whose pass remains the
  owner's to record.
- Validation notes `docs/validation/0001` onward document each
  slice with automated evidence kept separate from owner-heard
  evidence.
