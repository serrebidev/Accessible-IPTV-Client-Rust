# Validation 0031: follow-up pack (adapter) — Stalker DVR, updater install, gettext

Date: 2026-11-01. Program #54 follow-ups.

## Landed

- Stalker DVR: a scheduled recording whose channel is a Stalker
  portal entry (no direct URL) now resolves its stream link at
  record time through the provider worker and records with the
  resolved URL; a resolve failure fails the job with the error.
  The old "not supported yet" dead end is gone. Non-portal
  channels without a URL fail with a clear message.
- Updater install flow: the worker gained a Download job that
  fetches the manifest's package (installer asset when present,
  else the portable asset) into the config dir's updates/
  folder and verifies its SHA-256 before reporting success.
  When a check finds a newer version, Help > Check for Updates
  asks (Yes/No), downloads, and opens the verified package with
  the platform handler (explorer / open / xdg-open).
- Gettext sweep: UI strings now pass through the active i18n
  catalogue at the choke points — every menu label and help
  string (menu_add), every status announcement (Status::
  set_label, which also feeds the screen-reader announcement),
  and the shared list-dialog titles. The translator is the one
  built from the config language at startup; strings without a
  catalogue entry pass through unchanged, so behavior today is
  identical until .mo catalogues are dropped into the locale
  directory.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 122 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

A Stalker scheduled recording end to end, and a real update
download/open cycle, on Brandon's setup.
