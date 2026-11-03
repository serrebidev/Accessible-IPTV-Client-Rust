# Packaging the Rust client

Status: live. Brandon approved the workflow on 2026-11-01;
`.github/workflows/release.yml` implements this layout: per-OS
gates (fmt, Clippy, tests), a package per platform, and a publish
job that writes `update-manifest.json` and creates the release for
a `v*` tag.

## Local build

```
cargo build --release -p iptv-app
```

The binary is `target/release/iptv-app` (`iptv-app.exe` on Windows).
It runs with no beside-files: the English User Guide is compiled in,
and configuration is created on first save in the user's config
directory.

## Release layout (what the workflow produces)

One archive per platform, mirroring where the app looks for its
data files at runtime:

- the `iptv-app` binary;
- the bundled libVLC runtime beside the binary (the platform
  `libvlc` library, its core library, and a `plugins/` folder):
  the built-in player loads the bundled copy first, so playback
  works with no VLC install; a system VLC is only a fallback.
  Added 2026-11-02 after the v0.3.2 package shipped without it
  and the built-in player could not start (validation 0048);
- `docs/help/*.md` — currently English only. Translated guides remain
  to be ported. Executable-relative discovery also has a gap when
  the working directory differs (audit S08);
- gettext expects `locale/<lang>/LC_MESSAGES/iptvclient.mo` and
  `release_notes.mo`, compatible with the Python catalogues. The
  current Rust tree has no compiled catalogues, and release staging
  does not copy a locale directory. This is a packaging gap, not a
  shipped translation set; see the
  [feature parity audit](docs/feature-parity-audit.md#previously-documented-limits-still-relevant);
- `update-manifest.json` — the updater manifest asset, uploaded as
  a separate release asset next to the archive, with fields:
  `version`, `asset_filename`, `download_url`, `sha256` (of the
  archive), and an optional `installer` object
  (`asset`/`asset_name`/`asset_filename`, `download_url`, `sha256`).

## Update check contract

Since v0.4.0, updates run on ship-shape (the same signed
updater as BlindRadio). Help → Check for Updates — and the
optional launch check in Options — ask GitHub for the latest
release; when its version is newer, ship-shape requires the
platform asset below AND its `.minisig` sibling, downloads
both, and verifies the minisign signature against the public
key compiled into the app (assets/minisign.pub; the secret
half signs in the release workflow from the MINISIGN_KEY
Actions secret and lives in the owner's vault, never in
git). A release without signature pairs offers no update.

Asset names are fixed by ship-shape's contract and the
archive contents sit at the root (the updater extracts over
the app folder):

- `accessible-iptv-client.zip` — Windows (portable);
- `accessible-iptv-client.tar.gz` — Linux (portable);
- `accessible-iptv-client.dmg` — macOS. The macOS package is
  a bare unsigned binary rather than a signed app bundle, so
  ship-shape falls back to opening the verified disk image
  for a manual install there; Windows and Linux self-install
  and relaunch.

Transition: `update-manifest.json` is still published,
pointing at `accessible-iptv-client.zip`, so installs up to
v0.3.2 (the hand-rolled updater) can download the new
package and move onto the signed updater.
