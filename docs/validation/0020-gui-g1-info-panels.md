# Validation 0020: GUI parity G1 — main-window information surfaces

Date: 2026-11-01. Issue: #55 (program #54). Spec:
docs/gui-parity-audit.md; Python client_ui.py `_build_ui`.

## Landed

- The main frame now has a status bar; every status announcement
  also lands there (Status::set_label / set_silent mirror into the
  bar for the main window's Status only).
- Episode description read-only field after the channel list:
  moving through Channels fills it with the highlighted channel's
  current programme description (title when the programme has no
  description, cleared when no guide covers the channel).
- Stream URL read-only field after the description (Python's tab
  order): filled with the highlighted channel's URL; hidden when
  the `show_channel_url` setting is off (default on, as in Python).
- Layout factored into layout_main; panel construction into
  info_panels, keeping main() inside the pedantic line budget with
  no allow attributes.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 112 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass: tab order channels → Episode description → Stream URL,
description content on highlight, status bar presence. Panel
updates ride the existing selection event; compile-verified here.
