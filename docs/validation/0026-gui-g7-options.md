# Validation 0026: GUI parity G7 — options and system dialogs

Date: 2026-11-01. Issue: #61 (program #54).

## Landed

- Options > Keyboard Shortcuts... is live: every main/player
  command with its effective binding, Edit Binding (validated by
  the core engine with the Python error strings, default-restore
  on a blank entry), Reset All. Menus pick up new bindings on
  restart (announced in the dialog).
- Options > Preferred Audio Track... is live: keywords list
  (preferred_audio_tracks) and the audio-description check
  (prefer_audio_description), feeding slice 9's selection engine.
- Help > What's New is live (text updated per release).
- Per-control F1 help: F1 on Playlist view, Categories, Search,
  Channels, Episode description, and Stream URL opens the User
  Guide at that control's topic (guide::open_topic).
- Shutdown after recordings now opens the countdown dialog (30 s,
  Cancel) instead of shutting down immediately.
- Export/Import Settings, About, and the announcement radios
  landed in G2; with this slice every menu item is enabled.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass: shortcuts editor flow, F1 opening the right topics,
the countdown dialog's live label. Compile-verified here.
