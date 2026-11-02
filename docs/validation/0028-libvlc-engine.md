# Validation 0028: libVLC playback engine

Date: 2026-11-01. Issue: #49.

## Landed

- New crate iptv-vlc: libVLC loaded dynamically (dlopen) at first
  use — the workspace's one FFI exception, kept small and wrapped;
  the other crates keep forbid(unsafe_code). Absence of libVLC is
  a load error, never a startup failure.
- The built-in player tries libVLC first (as Python's does) and
  falls back to wx MediaCtrl per playback. Instance and player
  are created once and reused; reconnects from the session
  machine go through the engine too.
- Engine surface: play_url with header options (http-user-agent,
  http-referrer — libVLC 3 has no generic header option), play /
  pause / stop, seek, position/length, state, volume (0–100),
  audio track enumeration and selection.
- Video embedding: the media widget's native handle goes to
  libvlc_media_player_set_hwnd (Windows) / set_nsobject (macOS).
  On Linux libVLC opens its own video window (wx's GTK handle is
  a widget pointer, not an X window id) — recorded deviation.
- Adapter: position/length come from the active engine
  everywhere (resume checkpoints, subtitle speech and label,
  time label, seek slider); the 1 s tick polls the engine —
  resume seek on Playing, one-time track setup (choice filled
  from the engine; explicit preference wins, else the slice-9
  keyword engine), Ended/Error mapped into the session machine
  with reconnect. Pause/Stop menu items, player buttons, seek
  and volume sliders, and the track choice all act on the active
  engine.

## Automated evidence

- cargo fmt --check, clippy zero warnings across all three
  crates, 115 workspace tests pass (2 new: libVLC state mapping,
  graceful load without libVLC installed), playlist parity PASS,
  git diff --check clean.
- This VM has no libVLC installed, so the engine's live playback
  path is compile-verified plus the graceful-absence test.

## Owner-heard evidence still owed

Playback with VLC installed: audio track list and switching,
volume/seek against the engine, and end-of-stream behavior.
