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
- `docs/help/*.md` — guide translations as they land (the app
  prefers the on-disk file for the resolved language and falls back
  to the embedded English guide);
- `locale/<lang>/LC_MESSAGES/iptvclient.mo` and
  `release_notes.mo` — the gettext catalogues, shared with the
  Python product, which the i18n engine loads unchanged;
- `update-manifest.json` — the updater manifest asset, uploaded as
  a separate release asset next to the archive, with fields:
  `version`, `asset_filename`, `download_url`, `sha256` (of the
  archive), and an optional `installer` object
  (`asset`/`asset_name`/`asset_filename`, `download_url`, `sha256`).

## Update check contract

Help → Check for Updates reads the latest GitHub release of
`serrebidev/Accessible-IPTV-Client-Rust`, finds the
`update-manifest.json` asset (case-insensitive), and compares its
`version` against the running build. The environment's GitHub token
is only ever sent to GitHub hosts. Downloading and installing an
update in-app is not wired yet (the Python product's installer is a
Windows PowerShell helper with Authenticode pinning); the check
announces availability on the status line, and the core's verified
download helper (`download_file_sha256`) is ready for the install
step when it is ported.
