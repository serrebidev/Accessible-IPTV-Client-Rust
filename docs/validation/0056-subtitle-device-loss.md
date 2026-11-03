# 0056 — Subtitle speech device loss (issue #132)

When the application voice's chosen output device disappears,
the Rust client now follows Python's device-loss behavior
(`subtitle_speech.py` in the pinned reference 9aa89e05), driven
by the `subtitle_app_device_policy` config key.

## Policies

- "pause" (default): speech through the application voice
  pauses while the selected device is absent and resumes when
  it returns. The saved selection is never rewritten by a
  fallback.
- "default": the voice falls back to the system default output
  while the selected device is absent; the saved selection is
  retained and used again when the device returns.

## Core (wx-free, unit-tested)

`DeviceRouter` in `crates/iptv-core/src/speech.rs`:

- `validate` reports whether the selected device is present.
- `resolve` returns the route for the current device set:
  the exact saved token ID wins; a saved legacy device *name*
  migrates to its token ID only when exactly one present
  device carries that name (an ambiguous name never guesses);
  an absent selection yields Paused (pause policy) or
  DefaultOutput (default policy), with the saved selection
  retained either way.
- `CueSpeech::note_device_lost` reports the loss once per
  stretch through the screen reader, with the watchdog's text
  "The selected audio output is unavailable. Subtitle speech is
  paused." A speak attempted while paused reports Python's
  speak-path text once per stretch instead: "Subtitle speech
  paused: the selected audio output is unavailable."
  `note_route_healthy` re-arms both reports.

## Watchdog and application points

- The player tick runs the watchdog while an application-voice
  session with subtitles is playing (Python polls every
  500 ms): validate the output, apply the resolved route in
  the SAPI driver, purge speech when the route becomes Paused,
  and announce a fresh loss once.
- Switching the policy in the menu stops current speech,
  revalidates at once, and announces Python's lines ("If the
  output device disappears, subtitle speech pauses until it
  returns." / "...falls back to the system default.").
- Startup applies the saved output through the same router: a
  legacy name that migrates is persisted as its token ID, and
  an absent saved device is announced once when the
  application voice is the active backend.
- The output-choice preview (0055) validates through the same
  route, so previewing with an absent device fails the way
  Python's does.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 172 passed, 0 failed. The speech
  tests cover: exact-ID routing, unique-name legacy migration,
  ambiguous-name refusal, absent-device Paused/Default routes
  with the saved selection retained, once-per-stretch loss
  reporting for both texts, and re-arming on recovery.
- fmt clean, clippy clean, playlist parity PASS (see 0055).

## Not covered here

Physical device removal on Windows (unplugging a USB headset
mid-playback) needs owner hardware evidence; it is tracked by
the replacement-gate pack (#137) and is not claimed here.
