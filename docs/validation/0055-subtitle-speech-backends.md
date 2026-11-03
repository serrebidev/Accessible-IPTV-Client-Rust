# 0055 — Subtitle speech backends (issue #131)

The Rust client gains Python's two subtitle speech backends
(`app_tts.py` / `subtitle_speech.py` in the pinned reference
9aa89e05): the screen reader (default) and the application voice
(Windows SAPI), with exactly one backend per cue.

## Core (wx-free, unit-tested)

`crates/iptv-core/src/speech.rs`:

- `SpeechBackend` from the `subtitle_speech_backend` config key
  ("app_tts" selects the application voice; anything else is the
  screen reader, as in Python).
- `CueSpeech::route` decides per cue: screen reader, application
  voice, or suppressed (the output-preview window). Manual review
  (Read current / Read previous) rides the same path, so review
  works with automatic speech off, as in Python.
- `note_app_result` reports an application-voice failure through
  the screen reader once per failure stretch, with Python's text
  "Subtitle speech failed."; a healthy result re-arms the report.
- `set_backend` refuses the application voice where it is
  unavailable and changes nothing — the adapter then shows
  Python's dialog "The application voice is only available on
  Windows with SAPI." (title "Subtitle Speech").
- Speak flags mirror Python: SAPI async + purge + not-XML, so a
  new cue replaces the one still speaking (latest cue wins).

## Application voice driver

New workspace crate `iptv-sapi` (the wx-free boundary holds: the
COM driver sits behind the same stub/driver split as the rest of
the app):

- On Windows, raw COM `IDispatch` FFI mirroring Python's ctypes
  usage of `SAPI.SpVoice`: speak with purge, volume 0–100, rate
  −10..10, `GetAudioOutputs` enumeration, token-ID output
  selection via the `AudioOutput` property (PUTREF).
- `platform_available()` probes with a real create-and-release,
  the same test Python's backend construction performs.
- Off Windows the driver is unavailable and every call is a
  no-op failure, so the menu disables the application-voice
  items and the backend switch refuses, exactly as Python does
  without SAPI.

## Desktop wiring

- Options → Subtitle Speech now follows Python's layout: backend
  radios, Speak subtitles, Read current / Read previous, Review
  recent subtitles, then Application voice volume / output /
  speed, the device-policy radios (0056), and Subtitle
  capabilities. The capabilities text now names the application
  voice and the device policy.
- Startup applies the saved backend, volume, rate and output; a
  saved output that no longer exists is reported once (0056).
- The application voice is purged wherever Python stops speech:
  playback stop, channel change, seek, Speak subtitles switched
  off (main menu and player menu), backend switch, and policy
  change.
- Volume and speed use modal slider dialogs carrying Python's
  captions ("Volume:", "Speech speed (-10 to 10; 0 is
  normal):"); a rejected speed shows "Could not change the
  application voice speed."
- Output choice mirrors Python's preview flow: pick a device,
  hear "This is the application voice." with cues suppressed,
  then "Keep this output device?" — Yes persists and announces
  "Application voice output: {name}"; No restores the exact
  prior choice. Duplicate device names are numbered as Python
  numbers them.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 172 passed, 0 failed (8 new core
  speech tests cover routing, suppression, once-per-stretch
  failure reporting, backend refusal, and the device router of
  0056).
- `cargo check -p iptv-sapi --target x86_64-pc-windows-gnu`:
  clean — the Windows COM driver compiles for its target.
- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, playlist parity check PASS against the pinned Python.

## Not covered here

Owner-heard NVDA/SAPI evidence on a Windows machine is still
outstanding and is tracked by the replacement-gate pack (#137);
nothing in this document claims it. The SAPI driver compiles for
Windows but cannot run on this Linux host.
