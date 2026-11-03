# 0050 — Signed updates via ship-shape (plan; implementation follows)

Owner order 2026-11-02: "I also want this using shipfitter
like BLindRadio does, with this private signing key. This is
not to go in the repo, keep it private."

## Key facts (all verified 2026-11-02)
- The pasted key is a minisign secret in "encrypted" storage
  format whose KDF parameters are zeroed: it loads with NO
  password (`into_unencrypted_secret_key`), signs, and the
  signature verifies against BlindRadio's public key. It is
  the SAME keypair as BlindRadio's (public key id
  151E848B8939A63F; the owner's Vaultwarden item says so).
- The secret lives ONLY at ~/.config/iptv-signing/minisign.key
  (mode 600, outside every repo) and as the GitHub Actions
  secret MINISIGN_KEY on this repo (set 2026-11-03T01:19Z).
  It must never be committed, echoed, or copied into the
  workspace.
- BlindRadio's stack: shipfitter 0.2.4 (packaging lib),
  ship-shape 0.5.0 (updater), minisign 0.10.0 (signing),
  minisign-verify 0.2.5 (client verify). Its app entry is
  crates/blindradio/src/update.rs: UpdaterConfig::new(repo,
  app_name, display, PUBLIC_KEY, version) +
  ship_shape::ui::run_update_check.

## Ship-shape's asset contract (from the 0.5.0 source)
- Asset pair required: the asset plus a sibling
  `<asset>.minisig`; no pair, no update offered. Matching is
  case-insensitive. Version compare: release tag semver vs
  the running version; release body becomes the notes.
- Names derive from app_name + InstallKind + suffix:
  Windows Portable `{app}.zip`, Installer `{app}_setup.exe`;
  Linux Portable `{app}.tar.gz`, Installer `{app}.AppImage`;
  macOS always `{app}.dmg` (kind ignored).
- IPTV ships no installer/AppImage/dmg today, so:
  app_name = "accessible-iptv-client", InstallKind::Portable
  on Windows/Linux, and the macOS job additionally produces
  accessible-iptv-client.dmg from the staged folder
  (hdiutil) so macOS updates work too.

## Changes
1. App: add ship-shape 0.5.0 to iptv-app; new
   assets/minisign.pub (the PUBLIC key file — same as
   BlindRadio's, safe to commit); extend build.rs (existing,
   Linux link fix) to export line 2 of that file as
   IPTV_MINISIGN_PUBLIC_KEY. Replace the hand-rolled update
   flow: run_update_check and the dialog.rs update dialog
   call ship_shape::ui::run_update_check(config, frame,
   Stable, trigger); the core updater module
   (iptv-core::updater) and its dialog machinery are removed
   if nothing else uses them.
2. Release workflow: package assets renamed to
   accessible-iptv-client.zip (Windows),
   accessible-iptv-client.tar.gz (Linux),
   accessible-iptv-client.dmg (macOS, new). Publish job
   signs every asset with a small workspace signer bin
   (crates/iptv-signer, minisign 0.10.0; key from env
   MINISIGN_KEY, unencrypted load) and uploads the .minisig
   siblings; publish refuses when a pair is missing.
3. Transition: update-manifest.json keeps being published,
   pointing at the new Windows zip name, so installs up to
   v0.3.2 (hand-rolled updater) can still update forward
   onto v0.4.0. PACKAGING.md's contract section is
   rewritten to match.
4. Version: ships as v0.4.0 (updater swap is a feature).

## Order of work
After v0.3.3 (libVLC) and v0.3.4 (guide sequencing) land —
this slice rewrites the same workflow they use.
