# Validation 0027: GUI parity G4 — player window

Date: 2026-11-01. Issue: #58 (program #54).

## Landed

- Player window rebuilt: video surface, Seek slider (position /
  length), time label, subtitle label (current cue text), Volume
  slider (0–100, drives MediaCtrl volume), Choose Audio Track
  choice (records the preference; enumeration and application
  arrive with the libVLC engine, #49), and Pause/Stop/Record/
  Cast/Full Screen/Load subtitles buttons. Record and Cast act on
  the playing channel through the shared main wiring.
- A 1 s tick now refreshes the time label, seek position and
  subtitle label alongside the existing resume checkpoint and
  subtitle speech.
- Player shortcut dispatch is live: keys on the player window are
  matched against the effective player bindings (slice 12) —
  play/pause, stop, record, cast, volume up/down, audio track
  focus, subtitle load, subtitle speech toggle, read/previous/
  next subtitle announcements, full screen, hide, exit, and
  what's-playing.
- Cast window opening factored (show_cast_for) so the main window
  and the player window share it.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass over the player window: slider names and values, the
subtitle label, shortcut behavior (notably Up/Down volume versus
default slider keys), and the full-screen toggle. Compile-
verified here.
