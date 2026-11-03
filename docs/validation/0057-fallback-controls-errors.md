# 0057 — Fallback controls and errors accessible (issue #129)

The fallback engine is the basic player (`MediaCtrl`), used
when libVLC cannot start. Python's internal player is
libVLC-only, so this surface is judged against the basic
player's actual capabilities, as the ticket directs.

## Audit of the fallback surface (before this change)

Supported and already wired to `MediaCtrl`: play/pause, stop,
seek, volume, sidecar subtitle cues (display, speech, Read
current/previous), and buffered playback (the buffer clock
already read the control's position/state). Those stay enabled
and unchanged.

Three defects found:

1. **The session never heard from the fallback engine.**
   `poll_vlc_state` returns early when libVLC is inactive and
   nothing else fed the session machine, so under the basic
   player a healthy stream was never confirmed Playing (the
   #128 rule) and sat under the 30 s startup deadline until it
   was declared lost; a stream that ended or failed mid-play
   produced no Ended/Stalled either.
2. **VLC-only controls gave no honest explanation.** The Audio
   Track submenu kept whatever the previous play left in it —
   including working-looking track radios after a fallback
   play — and the Audio Output Device dialog claimed "No audio
   output devices were found." when the real problem was that
   the basic player cannot choose a device at all.
3. The play-time status line already explained the fallback
   ("libVLC is not available (...); basic player in use, many
   streams will not play.") — kept as is. The error text is the
   library loader's message: a file path, never a stream URL or
   credential.

## Changes

- `poll_fallback_state` on the player tick mirrors
  `poll_vlc_state`: Playing on the control's own confirmation
  only (with the pending resume seek applied), note_buffering
  fed the same way, and a Stopped after confirmed playback
  becomes Ended near the end or Stalled otherwise, entering
  the session's bounded recovery and Stream lost reporting.
  A Stopped *before* any confirmation is deliberately not fed:
  `MediaCtrl` also reports Stopped while a load is in flight,
  so feeding it would burn the reconnect budget on healthy
  loads; failed starts belong to the session's startup
  deadline, which is the designed path. (This guard lives in
  the wx adapter and cannot be unit-tested without a live
  control; the session behaviors it feeds — deadline, bounded
  retries, lost report — are covered by the #128 core tests.)
- `apply_engine_to_playback_menu` runs at every play: under
  the full player the Audio Output Device item is enabled and
  the track placeholder reads "No audio tracks available";
  under the basic player the device item is disabled, stale
  track radios and the window's stale track list are cleared,
  and the placeholder reads "Audio tracks are not available
  with the basic player" — the explanation sits in the menu
  where a keyboard user meets it.
- The Audio Output Device dialog now distinguishes the two
  empty cases: no libVLC at all gets "Audio output devices can
  only be chosen with the full player. The basic player is in
  use because libVLC is not available."; a present engine with
  no devices keeps the original wording.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 172 passed, 0 failed.
- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, playlist parity PASS against the pinned Python.

## Not covered here

Live fallback playback with NVDA (traversal, names/states,
focus restoration, a real failed start) needs a Windows
machine without libVLC and owner ears; it is tracked by the
replacement-gate pack (#137) and is not claimed here.
